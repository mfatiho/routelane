#![allow(dead_code)]
use anyhow::{bail, Context, Result};
/// DNS tabanlı yönlendirme — dnsmasq ipset + iptables fwmark
///
/// Neden ip rule + çözümlenen IP yerine bu?
///   - chatgpt.com Cloudflare CDN'den geliyor; her DNS yanıtı farklı IP seti döndürür
///   - dnsmasq, her DNS yanıtında IPs'leri kernel ipset'e otomatik ekler
///   - Tek bir ip-rule (fwmark bazlı) tüm domainleri karşılar
///   - Kernel ipset timeout ile eski CDN IP'lerini otomatik temizler
///
/// Sistem gereksinimleri:
///   - dnsmasq ≥ 2.86 ("ipset" derleme seçeneğiyle)
///   - ipset / xt_set kernel modülleri
///   - iptables-nft (Ubuntu 22.04 varsayılan)
///   - Sistemin DNS resolver'ı dnsmasq'tan geçmeli (kurulum adımlarına bak)
use std::fs;
use std::path::Path;
use std::process::Command;

// ──────────────────────────────────────────────────────────────────────────────
// Sabitler
// ──────────────────────────────────────────────────────────────────────────────

pub const IPSET_V4: &str = "routelane4";
pub const IPSET_V6: &str = "routelane6";
pub const FWMARK: u32 = 0x726c; // 'rl' — routelane
pub const DNSMASQ_CONF_DIR: &str = "/etc/dnsmasq.d";
pub const ROUTELANE_TABLE: u32 = 100;

// ──────────────────────────────────────────────────────────────────────────────
// Durum
// ──────────────────────────────────────────────────────────────────────────────

pub struct DnsRouter {
    /// Şu an yönetilen domain listesi (config dosyaları /etc/dnsmasq.d/routelane-*.conf)
    domains: Vec<String>,
    /// fwmark ip-rule ve iptables kuralları yüklendi mi?
    kernel_rules_active: bool,
}

impl DnsRouter {
    pub fn new() -> Self {
        Self {
            domains: Vec::new(),
            kernel_rules_active: false,
        }
    }

    // ── Domain ekleme ────────────────────────────────────────────────────────

    /// Domain'i dnsmasq ipset konfigürasyonuna ekler.
    /// İlk domaine kadar kernel altyapısını kurar (ipset + iptables + ip rule).
    pub fn add_domain(&mut self, domain: &str, alt_iface: &str) -> Result<()> {
        if !self.kernel_rules_active {
            self.setup_kernel(alt_iface)?;
        }

        let config_path = dnsmasq_conf_path(domain);
        write_dnsmasq_config(domain, &config_path)?;
        reload_dnsmasq().context("dnsmasq yeniden yükleme başarısız")?;

        if !self.domains.contains(&domain.to_owned()) {
            self.domains.push(domain.to_owned());
        }
        Ok(())
    }

    /// Domain'i konfigürasyondan kaldırır ve dnsmasq'ı yeniden yükler.
    pub fn remove_domain(&mut self, domain: &str) -> Result<()> {
        let config_path = dnsmasq_conf_path(domain);
        if config_path.exists() {
            fs::remove_file(&config_path)
                .with_context(|| format!("{:?} silinemedi", config_path))?;
            reload_dnsmasq()?;
        }
        self.domains.retain(|d| d != domain);
        Ok(())
    }

    // ── Sıfırlama ────────────────────────────────────────────────────────────

    /// Tüm routelane konfigürasyonunu temizler — idempotent.
    pub fn reset_all(&mut self) -> Result<()> {
        // Dnsmasq config dosyalarını kaldır
        self.remove_all_dnsmasq_configs();

        // Kernel kurallarını temizle
        self.teardown_kernel().ok(); // hata olsa da devam et

        self.domains.clear();
        self.kernel_rules_active = false;
        Ok(())
    }

    /// Sistemde routelane izi var mı? (root gerektirmez)
    pub fn has_leftovers() -> bool {
        // ip rule kontrol
        let has_rule = Command::new("ip")
            .args(["rule", "list"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("fwmark 0x{:x}", FWMARK)))
            .unwrap_or(false);

        // config dosyası kontrol
        let has_conf = fs::read_dir(DNSMASQ_CONF_DIR)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .any(|e| e.file_name().to_string_lossy().starts_with("routelane-"))
            })
            .unwrap_or(false);

        has_rule || has_conf
    }

    // ── Kernel altyapısı ─────────────────────────────────────────────────────

    fn setup_kernel(&mut self, alt_iface: &str) -> Result<()> {
        // 1. ipset oluştur (zaten varsa EEXIST — görmezden gel)
        create_ipset(IPSET_V4, "inet")?;
        create_ipset(IPSET_V6, "inet6")?;

        // 2. Routing tablosuna alt arayüz default route ekle
        let gateway = crate::routing::executor::detect_gateway(alt_iface)
            .with_context(|| format!("'{}' için gateway bulunamadı", alt_iface))?;

        run_ip_silent(&[
            "route",
            "add",
            "default",
            "via",
            &gateway,
            "dev",
            alt_iface,
            "table",
            &ROUTELANE_TABLE.to_string(),
        ]);

        // 3. iptables fwmark kuralı — src paketler (OUTPUT)
        add_iptables_mark_rule("-A", "OUTPUT")?;
        add_iptables_mark_rule("-A", "PREROUTING")?;

        // 4. ip rule: fwmark → lookup 100
        run_ip_silent(&[
            "rule",
            "add",
            "fwmark",
            &format!("0x{:x}", FWMARK),
            "lookup",
            &ROUTELANE_TABLE.to_string(),
            "priority",
            "10000",
        ]);

        self.kernel_rules_active = true;
        log::info!(
            "DNS router kernel altyapısı kuruldu (fwmark=0x{:x}, table={})",
            FWMARK,
            ROUTELANE_TABLE
        );
        Ok(())
    }

    fn teardown_kernel(&self) -> Result<()> {
        // ip rule sil
        run_ip_silent(&["rule", "del", "fwmark", &format!("0x{:x}", FWMARK)]);

        // iptables kurallarını sil
        add_iptables_mark_rule("-D", "OUTPUT").ok();
        add_iptables_mark_rule("-D", "PREROUTING").ok();

        // ip route flush
        run_ip_silent(&["route", "flush", "table", &ROUTELANE_TABLE.to_string()]);

        // ipset temizle ve sil
        Command::new("ipset")
            .args(["flush", IPSET_V4])
            .output()
            .ok();
        Command::new("ipset")
            .args(["flush", IPSET_V6])
            .output()
            .ok();
        Command::new("ipset")
            .args(["destroy", IPSET_V4])
            .output()
            .ok();
        Command::new("ipset")
            .args(["destroy", IPSET_V6])
            .output()
            .ok();

        Ok(())
    }

    fn remove_all_dnsmasq_configs(&self) {
        if let Ok(entries) = fs::read_dir(DNSMASQ_CONF_DIR) {
            for entry in entries.filter_map(|e| e.ok()) {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with("routelane-") && name_str.ends_with(".conf") {
                    fs::remove_file(entry.path()).ok();
                }
            }
        }
        reload_dnsmasq().ok();
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Yardımcı fonksiyonlar (root gerektirir — executor'da çağrılacak)
// ──────────────────────────────────────────────────────────────────────────────

fn dnsmasq_conf_path(domain: &str) -> std::path::PathBuf {
    // Dosya adında tehlikeli karakterlere izin vermemek için sanitize et
    let safe_name: String = domain
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    Path::new(DNSMASQ_CONF_DIR).join(format!("routelane-{}.conf", safe_name))
}

fn write_dnsmasq_config(domain: &str, path: &std::path::PathBuf) -> Result<()> {
    // dnsmasq ipset direktifi:
    // ipset=/<domain>/<ipset_v4>,<ipset_v6>
    // Her DNS yanıtında eşleşen IPs kernel ipset'e eklenir (timeout ile)
    let content = format!(
        "# routelane: {} → ipset\nipset=/{}/{},{}\n",
        domain, domain, IPSET_V4, IPSET_V6
    );
    fs::write(path, &content).with_context(|| format!("{:?} yazılamadı", path))
}

fn reload_dnsmasq() -> Result<()> {
    // SIGHUP dnsmasq'ın config'i yeniden yüklemesini sağlar
    let output = Command::new("pkill")
        .args(["-HUP", "dnsmasq"])
        .output()
        .context("pkill başlatılamadı")?;

    if !output.status.success() {
        // dnsmasq çalışmıyorsa başlat
        Command::new("dnsmasq")
            .args(["--conf-dir=/etc/dnsmasq.d,*.conf"])
            .output()
            .ok();
    }
    Ok(())
}

fn create_ipset(name: &str, family: &str) -> Result<()> {
    // "hash:ip timeout 300" → IP'ler 5 dakika sonra otomatik sona erer
    // CDN IP rotasyonuna karşı koruma
    let output = Command::new("ipset")
        .args([
            "create", name, "hash:ip", "timeout", "300", "family", family,
        ])
        .output()
        .context("ipset create başlatılamadı")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // "set with the same name already exists" → kabul et
        if !stderr.contains("already exists") {
            bail!("ipset create {} başarısız: {}", name, stderr.trim());
        }
    }
    Ok(())
}

fn add_iptables_mark_rule(action: &str, chain: &str) -> Result<()> {
    // iptables -t mangle -A OUTPUT -m set --match-set routelane4 dst -j MARK --set-mark 0x726c
    let mark_str = format!("0x{:x}", FWMARK);
    let output = Command::new("iptables")
        .args([
            "-t",
            "mangle",
            action,
            chain,
            "-m",
            "set",
            "--match-set",
            IPSET_V4,
            "dst",
            "-j",
            "MARK",
            "--set-mark",
            &mark_str,
        ])
        .output()
        .context("iptables başlatılamadı")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("iptables {} {} başarısız: {}", action, chain, stderr.trim());
    }
    Ok(())
}

fn run_ip_silent(args: &[&str]) {
    Command::new("ip").args(args).output().ok();
}
