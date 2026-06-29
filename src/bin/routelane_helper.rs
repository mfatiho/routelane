// routelane-helper — pkexec tarafından root olarak çalıştırılan ayrıcalıklı yardımcı
//
// GÜVENLİK İLKELERİ:
//   1. Başlar başlamaz UID 0 olduğunu doğrular; değilse çıkar.
//   2. stdin'den gelen JSON'u HelperRequest tipine deserialize eder.
//   3. Her HelperOp'u katı bir beyaz liste kuralına göre doğrular.
//   4. Yalnızca sabit tablo numarası (100–199) ve öncelik aralığı
//      (10000–10999) kabul eder — sabit sınırlar dışındaki değerleri reddeder.
//   5. Hiçbir şekilde arbitrary shell komutu çalıştırmaz.
//   6. Sonucu JSON olarak stdout'a yazar; hataları stderr'e basar.

use std::io::{self, Read};
use std::process::Command;

// Bu dosya ana binary'nin models modülünü paylaşmaz; bağımsız minimaL yapılar kullanır.
// Amaç: helper'ı mümkün olduğunca küçük ve denetlenebilir tutmak.

use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────────────────────
// Paylaşılan IPC tipleri (models.rs'den kopyalanmış — bağımlılık önlemek için)
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum HelperOp {
    // Klasik ip-rule backend
    AddRoute {
        gateway: String,
        interface: String,
        table: u32,
        family: IpFamily,
    },
    AddRule {
        destination: String,
        table: u32,
        priority: u32,
        family: IpFamily,
    },
    FlushRulesInRange {
        min_priority: u32,
        max_priority: u32,
        family: IpFamily,
    },
    FlushTable {
        table: u32,
        family: IpFamily,
    },
    // ipset + fwmark backend
    IpsetCreate {
        name: String,
        family: String,
        timeout_secs: u32,
    },
    IpsetAdd {
        name: String,
        ip: String,
    },
    IpsetDestroy {
        name: String,
    },
    IptablesMark {
        chain: String,
        action: String,
        ipset_name: String,
        mark: u32,
    },
    AddFwmarkRule {
        mark: u32,
        table: u32,
        priority: u32,
    },
    DelFwmarkRule {
        mark: u32,
    },
    WriteDnsmasqEntry {
        domain: String,
    },
    RemoveDnsmasqEntry {
        domain: String,
    },
    ReloadDnsmasq,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum IpFamily {
    V4,
    V6,
}

#[derive(Debug, Deserialize)]
struct HelperRequest {
    ops: Vec<HelperOp>,
}

#[derive(Debug, Serialize)]
struct HelperResponse {
    success: bool,
    errors: Vec<String>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Beyaz liste sabitleri
// ──────────────────────────────────────────────────────────────────────────────

const TABLE_MIN: u32 = 100;
const TABLE_MAX: u32 = 199;
const PRIO_MIN: u32 = 10_000;
const PRIO_MAX: u32 = 10_999;

fn main() {
    // ── Güvenlik denetimi: root olarak çalışmalıyız ───────────────────────────
    let uid = libc_getuid();
    if uid != 0 {
        eprintln!("HATA: routelane-helper root olarak çalışmalıdır (pkexec ile)");
        std::process::exit(1);
    }

    // ── stdin'den JSON oku ────────────────────────────────────────────────────
    let mut input = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut input) {
        write_error_response(&format!("stdin okuma hatası: {}", e));
        std::process::exit(1);
    }

    let request: HelperRequest = match serde_json::from_str(&input) {
        Ok(r) => r,
        Err(e) => {
            write_error_response(&format!("JSON ayrıştırma hatası: {}", e));
            std::process::exit(1);
        }
    };

    // ── Komutları çalıştır ────────────────────────────────────────────────────
    let mut errors = Vec::new();

    for op in &request.ops {
        if let Err(e) = execute_op(op) {
            errors.push(e);
        }
    }

    let response = HelperResponse {
        success: errors.is_empty(),
        errors,
    };

    let response_json = serde_json::to_string(&response).unwrap_or_default();
    println!("{}", response_json);

    if !response.success {
        std::process::exit(1);
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Komut yürütme — her op için ayrı doğrulama + `ip` çağrısı
// ──────────────────────────────────────────────────────────────────────────────

fn execute_op(op: &HelperOp) -> Result<(), String> {
    match op {
        HelperOp::AddRoute {
            gateway,
            interface,
            table,
            family,
        } => {
            validate_table(*table)?;
            validate_ip_or_iface(gateway, "gateway")?;
            validate_ip_or_iface(interface, "interface")?;
            run_ip(
                *family,
                &[
                    "route",
                    "replace",
                    "default",
                    "via",
                    gateway,
                    "dev",
                    interface,
                    "table",
                    &table.to_string(),
                ],
            )
        }

        HelperOp::AddRule {
            destination,
            table,
            priority,
            family,
        } => {
            validate_table(*table)?;
            validate_priority(*priority)?;
            validate_cidr(destination, *family)?;
            run_ip(
                *family,
                &[
                    "rule",
                    "add",
                    "to",
                    destination,
                    "lookup",
                    &table.to_string(),
                    "priority",
                    &priority.to_string(),
                ],
            )
        }

        HelperOp::FlushRulesInRange {
            min_priority,
            max_priority,
            family,
        } => {
            // Kısmi öncelik aralığına da izin ver (tek bir kuralı silmek için)
            if *min_priority < PRIO_MIN || *max_priority > PRIO_MAX {
                return Err(format!(
                    "Öncelik aralığı [{},{}] izin verilen [{},{}] dışında",
                    min_priority, max_priority, PRIO_MIN, PRIO_MAX
                ));
            }
            flush_rules_in_range(*family, *min_priority, *max_priority)
        }

        HelperOp::FlushTable { table, family } => {
            validate_table(*table)?;
            run_ip(*family, &["route", "flush", "table", &table.to_string()]).or(Ok(()))
        }

        // ── ipset ───────────────────────────────────────────────────────────
        HelperOp::IpsetCreate {
            name,
            family,
            timeout_secs,
        } => {
            validate_ipset_name(name)?;
            if !matches!(family.as_str(), "inet" | "inet6") {
                return Err(format!("Geçersiz ipset family: {}", family));
            }
            if *timeout_secs > 86400 {
                return Err("timeout_secs çok büyük (max 86400)".to_owned());
            }
            let ts = timeout_secs.to_string();
            let output = Command::new("/sbin/ipset")
                .args(["create", name, "hash:ip", "timeout", &ts, "family", family])
                .output()
                .map_err(|e| format!("ipset create: {}", e))?;
            if !output.status.success() {
                let s = String::from_utf8_lossy(&output.stderr);
                if s.contains("already exists") {
                    return Ok(());
                }
                return Err(format!("ipset create {}: {}", name, s.trim()));
            }
            Ok(())
        }

        HelperOp::IpsetAdd { name, ip } => {
            validate_ipset_name(name)?;
            validate_cidr(ip, IpFamily::V4).or_else(|_| validate_cidr(ip, IpFamily::V6))?;
            run_cmd("/sbin/ipset", &["add", name, ip, "-exist"])
        }

        HelperOp::IpsetDestroy { name } => {
            validate_ipset_name(name)?;
            Command::new("/sbin/ipset")
                .args(["flush", name])
                .output()
                .ok();
            Command::new("/sbin/ipset")
                .args(["destroy", name])
                .output()
                .ok();
            Ok(())
        }

        // ── iptables fwmark ─────────────────────────────────────────────────
        HelperOp::IptablesMark {
            chain,
            action,
            ipset_name,
            mark,
        } => {
            if !matches!(chain.as_str(), "OUTPUT" | "PREROUTING") {
                return Err(format!("Geçersiz chain: {}", chain));
            }
            if !matches!(action.as_str(), "-A" | "-D") {
                return Err(format!("Geçersiz action: {}", action));
            }
            validate_ipset_name(ipset_name)?;
            let mark_str = format!("0x{:x}", mark);
            run_cmd(
                "/sbin/iptables",
                &[
                    "-t",
                    "mangle",
                    action,
                    chain,
                    "-m",
                    "set",
                    "--match-set",
                    ipset_name,
                    "dst",
                    "-j",
                    "MARK",
                    "--set-mark",
                    &mark_str,
                ],
            )
        }

        HelperOp::AddFwmarkRule {
            mark,
            table,
            priority,
        } => {
            validate_table(*table)?;
            run_ip(
                IpFamily::V4,
                &[
                    "rule",
                    "add",
                    "fwmark",
                    &format!("0x{:x}", mark),
                    "lookup",
                    &table.to_string(),
                    "priority",
                    &priority.to_string(),
                ],
            )
        }

        HelperOp::DelFwmarkRule { mark } => run_ip(
            IpFamily::V4,
            &["rule", "del", "fwmark", &format!("0x{:x}", mark)],
        )
        .or(Ok(())),

        // ── dnsmasq config ──────────────────────────────────────────────────
        HelperOp::WriteDnsmasqEntry { domain } => {
            validate_domain(domain)?;
            let path = format!("/etc/dnsmasq.d/routelane-{}.conf", domain);
            let content = format!(
                "# routelane managed — do not edit\nipset=/{}/routelane4,routelane6\n",
                domain
            );
            std::fs::write(&path, content)
                .map_err(|e| format!("dnsmasq config yazılamadı {}: {}", path, e))
        }

        HelperOp::RemoveDnsmasqEntry { domain } => {
            validate_domain(domain)?;
            let path = format!("/etc/dnsmasq.d/routelane-{}.conf", domain);
            std::fs::remove_file(&path).or(Ok(()))
        }

        HelperOp::ReloadDnsmasq => {
            let out = Command::new("/usr/bin/pkill")
                .args(["-HUP", "dnsmasq"])
                .output()
                .map_err(|e| format!("pkill: {}", e))?;
            // Çalışmıyorsa başlat
            if !out.status.success() {
                Command::new("/usr/sbin/dnsmasq").output().ok();
            }
            Ok(())
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Doğrulama yardımcıları
// ──────────────────────────────────────────────────────────────────────────────

fn validate_table(table: u32) -> Result<(), String> {
    if table < TABLE_MIN || table > TABLE_MAX {
        return Err(format!(
            "Tablo {} izin verilen aralık [{},{}] dışında",
            table, TABLE_MIN, TABLE_MAX
        ));
    }
    Ok(())
}

fn validate_priority(prio: u32) -> Result<(), String> {
    if prio < PRIO_MIN || prio > PRIO_MAX {
        return Err(format!(
            "Öncelik {} izin verilen [{},{}] dışında",
            prio, PRIO_MIN, PRIO_MAX
        ));
    }
    Ok(())
}

/// IP adresi veya ağ arayüzü adının yalnızca güvenli karakterler içerdiğini doğrular.
/// Shell injection'ı önler.
fn validate_ip_or_iface(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 64 {
        return Err(format!("{} boş veya çok uzun", field));
    }
    let valid = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '/' | '-' | '_' | '@'));
    if !valid {
        return Err(format!("{} geçersiz karakter içeriyor: {:?}", field, value));
    }
    Ok(())
}

/// CIDR notasyonunu doğrular: "192.168.1.0/24" veya "2001:db8::/32"
fn validate_cidr(value: &str, family: IpFamily) -> Result<(), String> {
    validate_ip_or_iface(value, "destination")?;
    let (ip, prefix) = value
        .split_once('/')
        .map(|(ip, prefix)| (ip, Some(prefix)))
        .unwrap_or((value, None));

    let parsed: std::net::IpAddr = ip
        .parse()
        .map_err(|_| format!("Geçersiz IP adresi: {}", ip))?;
    let is_expected_family = matches!(
        (family, parsed),
        (IpFamily::V4, std::net::IpAddr::V4(_)) | (IpFamily::V6, std::net::IpAddr::V6(_))
    );
    if !is_expected_family {
        return Err(format!("IP ailesi uyuşmuyor: {}", value));
    }

    if let Some(prefix) = prefix {
        let p: u8 = prefix
            .parse()
            .map_err(|_| format!("Geçersiz CIDR prefix: {}", prefix))?;
        let max = match family {
            IpFamily::V4 => 32,
            IpFamily::V6 => 128,
        };
        if p > max {
            return Err(format!("CIDR prefix {} çok büyük", p));
        }
    }
    Ok(())
}

fn validate_ipset_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 31 {
        return Err(format!("ipset adı geçersiz uzunluk: {:?}", name));
    }
    // Sadece routelane4, routelane6 ve benzeri güvenli isimlere izin ver
    if !name.starts_with("routelane") {
        return Err(format!("ipset adı 'routelane' ile başlamalı: {:?}", name));
    }
    let valid = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !valid {
        return Err(format!("ipset adı geçersiz karakter içeriyor: {:?}", name));
    }
    Ok(())
}

fn validate_domain(domain: &str) -> Result<(), String> {
    if domain.is_empty() || domain.len() > 253 {
        return Err("domain geçersiz uzunluk".to_owned());
    }
    let valid = domain
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if !valid {
        return Err(format!("domain geçersiz karakter: {:?}", domain));
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Komut yürütücüler
// ──────────────────────────────────────────────────────────────────────────────

fn run_cmd(bin: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(bin)
        .args(args)
        .output()
        .map_err(|e| format!("{} başlatılamadı: {}", bin, e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{} {} başarısız: {}",
            bin,
            args.join(" "),
            stderr.trim()
        ));
    }
    Ok(())
}

fn run_ip(family: IpFamily, args: &[&str]) -> Result<(), String> {
    let family_arg = match family {
        IpFamily::V4 => "-4",
        IpFamily::V6 => "-6",
    };
    let output = Command::new("/sbin/ip") // tam yol — PATH'e güvenmiyoruz
        .arg(family_arg)
        .args(args)
        .output()
        .map_err(|e| format!("ip komutu başlatılamadı: {}", e))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "ip {} başarısız ({}): {}",
        args.join(" "),
        output.status,
        stderr.trim()
    ))
}

/// Bir öncelik aralığındaki tüm ip rule'ları siler (idempotent).
/// Aynı öncelikte birden fazla kural olabilir; hepsini silmek için döngü kullanır.
fn flush_rules_in_range(family: IpFamily, min_prio: u32, max_prio: u32) -> Result<(), String> {
    loop {
        let family_arg = match family {
            IpFamily::V4 => "-4",
            IpFamily::V6 => "-6",
        };
        let output = Command::new("/sbin/ip")
            .args([family_arg, "--json", "rule", "list"])
            .output()
            .map_err(|e| format!("ip rule list başarısız: {}", e))?;

        let rules: Vec<serde_json::Value> =
            serde_json::from_slice(&output.stdout).unwrap_or_default();

        let targets: Vec<u32> = rules
            .iter()
            .filter_map(|r| r.get("priority")?.as_u64())
            .filter(|&p| p >= min_prio as u64 && p <= max_prio as u64)
            .map(|p| p as u32)
            .collect();

        if targets.is_empty() {
            break;
        }

        // Her öncelik için bir kez sil; aynı öncelikte birden fazlaysa
        // bir sonraki iterasyonda devam eder
        let mut any_deleted = false;
        for prio in &targets {
            if run_ip(family, &["rule", "del", "priority", &prio.to_string()]).is_ok() {
                any_deleted = true;
            }
        }
        if !any_deleted {
            break; // Sonsuz döngüyü önle
        }
    }
    Ok(())
}

fn write_error_response(msg: &str) {
    let response = HelperResponse {
        success: false,
        errors: vec![msg.to_owned()],
    };
    println!("{}", serde_json::to_string(&response).unwrap_or_default());
}

// libc getuid çağrısı — bağımlılık eklemeden
extern "C" {
    fn getuid() -> u32;
}

fn libc_getuid() -> u32 {
    unsafe { getuid() }
}
