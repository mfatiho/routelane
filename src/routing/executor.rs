use crate::models::{
    HelperOp, HelperRequest, HelperResponse, IpFamily, NetworkInterface, RouteProbe,
};
use anyhow::{bail, Context, Result};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// ──────────────────────────────────────────────────────────────────────────────
// Sabitler
// ──────────────────────────────────────────────────────────────────────────────

pub const ROUTELANE_TABLE: u32 = 100;
pub const PRIORITY_BASE: u32 = 10_000;
pub const PRIORITY_MAX: u32 = 10_999;
const POLICY_HELPER_PATH: &str = "/usr/lib/routelane/routelane-helper";
const LEGACY_HELPER_PATH: &str = "/usr/bin/routelane-helper";

// ──────────────────────────────────────────────────────────────────────────────
// Executor — tüm ayrıcalıklı komutları helper üzerinden toplu gönderir
// ──────────────────────────────────────────────────────────────────────────────

pub struct Executor {
    /// pkexec yolu; test ortamında sudo kullanmak için değiştirilebilir
    helper_path: String,
}

impl Executor {
    pub fn new() -> Self {
        let helper_path = resolve_helper_path();
        Self { helper_path }
    }

    /// Bir dizi HelperOp'u tek bir pkexec çağrısıyla çalıştırır.
    /// Polkit, ilk çağrıda şifre sorar; sonraki çağrılar (auth_admin_keep) şifresiz geçer.
    pub fn execute_batch(&self, ops: &[HelperOp]) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }

        let request = HelperRequest { ops: ops.to_vec() };
        let request_json =
            serde_json::to_string(&request).context("HelperRequest serileştirme hatası")?;

        let mut child = Command::new("pkexec")
            .arg(&self.helper_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("pkexec başlatılamadı — routelane-helper kurulu mu?")?;

        // JSON'u stdin'e yaz
        {
            use std::io::Write;
            let stdin = child.stdin.as_mut().context("stdin açılamadı")?;
            stdin.write_all(request_json.as_bytes())?;
        }

        let output = child.wait_with_output().context("helper bekleme hatası")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("routelane-helper başarısız: {}", stderr.trim());
        }

        let response: HelperResponse =
            serde_json::from_slice(&output.stdout).context("helper yanıtı ayrıştırılamadı")?;

        if !response.success {
            bail!("Komutlar kısmen başarısız: {}", response.errors.join("; "));
        }

        Ok(())
    }
}

fn resolve_helper_path() -> String {
    let env_path = std::env::var("ROUTELANE_HELPER").ok().map(PathBuf::from);
    let current_exe = std::env::current_exe().ok();

    resolve_helper_path_from(env_path, current_exe, |path| path.is_file())
}

fn resolve_helper_path_from(
    env_path: Option<PathBuf>,
    current_exe: Option<PathBuf>,
    is_file: impl Fn(&Path) -> bool,
) -> String {
    let sibling_path =
        current_exe.and_then(|path| path.parent().map(|dir| dir.join("routelane-helper")));

    let mut candidates = Vec::new();
    candidates.push(PathBuf::from(POLICY_HELPER_PATH));
    if let Some(path) = env_path.clone() {
        candidates.push(path);
    }
    if let Some(path) = sibling_path.clone() {
        candidates.push(path);
    }
    candidates.push(PathBuf::from(LEGACY_HELPER_PATH));

    if let Some(path) = candidates.iter().find(|path| is_file(path)) {
        return path_to_string(path);
    }

    let fallback = env_path
        .or(sibling_path)
        .unwrap_or_else(|| PathBuf::from(POLICY_HELPER_PATH));
    path_to_string(&fallback)
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

// ──────────────────────────────────────────────────────────────────────────────
// Root gerektirmeyen sorgular (normal kullanıcı olarak çalışır)
// ──────────────────────────────────────────────────────────────────────────────

/// Sistemde routelane priority aralığında kural var mı? (pkexec gerektirmez)
pub fn has_leftover_rules() -> bool {
    has_leftover_rules_for(&[]) || has_leftover_rules_for(&["-6"])
}

fn has_leftover_rules_for(family_args: &[&str]) -> bool {
    let mut args = family_args.to_vec();
    args.extend(["--json", "rule", "list"]);

    let Ok(output) = Command::new("ip").args(args).output() else {
        return false;
    };
    let rules: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap_or_default();

    rules.iter().any(|r| {
        r.get("priority")
            .and_then(|p| p.as_u64())
            .map(|p| p >= PRIORITY_BASE as u64 && p <= PRIORITY_MAX as u64)
            .unwrap_or(false)
    })
}

/// Sistemdeki ağ arayüzlerini ve gateway'lerini listeler
pub fn list_interfaces() -> Vec<NetworkInterface> {
    let Ok(output) = Command::new("ip").args(["--json", "link", "show"]).output() else {
        return vec![];
    };

    let links: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap_or_default();

    let default_iface = detect_default_interface();

    links
        .iter()
        .filter_map(|link| {
            let name = link.get("ifname")?.as_str()?.to_owned();
            if name == "lo" {
                return None;
            }
            let is_default = default_iface.as_deref() == Some(&name);
            let gateway = detect_gateway(&name);
            Some(NetworkInterface {
                name,
                gateway,
                is_default,
            })
        })
        .collect()
}

/// Sistem varsayılan route'unu sağlayan arayüzü döner
pub fn detect_default_interface() -> Option<String> {
    let output = Command::new("ip")
        .args(["--json", "route", "show", "default"])
        .output()
        .ok()?;
    let routes: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).ok()?;
    // En düşük metrikli varsayılan route'u seç
    routes
        .iter()
        .filter(|r| r.get("dst").and_then(|d| d.as_str()) == Some("default"))
        .min_by_key(|r| r.get("metric").and_then(|m| m.as_u64()).unwrap_or(u64::MAX))
        .and_then(|r| r.get("dev")?.as_str().map(str::to_owned))
}

/// Belirtilen arayüzün varsayılan gateway'ini bulur
pub fn detect_gateway(interface: &str) -> Option<String> {
    let output = Command::new("ip")
        .args(["--json", "route", "show", "dev", interface])
        .output()
        .ok()?;

    let routes: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).ok()?;

    routes.iter().find_map(|r| {
        if r.get("dst")?.as_str()? == "default" {
            r.get("gateway")?.as_str().map(str::to_owned)
        } else {
            None
        }
    })
}

/// Belirtilen arayüzün IPv6 varsayılan gateway'ini bulur
pub fn detect_gateway_v6(interface: &str) -> Option<String> {
    let output = Command::new("ip")
        .args(["-6", "--json", "route", "show", "default", "dev", interface])
        .output()
        .ok()?;

    let routes: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).ok()?;

    routes.iter().find_map(|r| {
        let is_default = r.get("dst").and_then(|d| d.as_str()) == Some("default")
            || r.get("dst").and_then(|d| d.as_str()) == Some("::/0");
        if is_default {
            r.get("gateway")?.as_str().map(str::to_owned)
        } else {
            None
        }
    })
}

/// Hedef IP için kernel'in seçeceği route'u döner. Root gerektirmez.
pub fn probe_route(ip: IpAddr) -> Result<RouteProbe> {
    let family = match ip {
        IpAddr::V4(_) => IpFamily::V4,
        IpAddr::V6(_) => IpFamily::V6,
    };
    let family_arg = match family {
        IpFamily::V4 => "-4",
        IpFamily::V6 => "-6",
    };
    let ip_str = ip.to_string();

    let output = Command::new("ip")
        .args([family_arg, "route", "get", &ip_str])
        .output()
        .with_context(|| format!("{} için rota sorgusu başlatılamadı", ip_str))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{} için rota bulunamadı: {}", ip_str, stderr.trim());
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let interface = parse_route_get_output(&raw);

    Ok(RouteProbe {
        destination: ip_str,
        family,
        interface,
    })
}

fn parse_route_get_output(raw: &str) -> Option<String> {
    let tokens: Vec<&str> = raw.split_whitespace().collect();

    for pair in tokens.windows(2) {
        if pair[0] == "dev" {
            return Some(pair[1].to_owned());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{parse_route_get_output, resolve_helper_path_from};
    use std::path::{Path, PathBuf};

    #[test]
    fn parses_route_get_output() {
        let dev =
            parse_route_get_output("142.250.184.206 via 192.168.1.1 dev wlan0 src 192.168.1.25");
        assert_eq!(dev.as_deref(), Some("wlan0"));
    }

    #[test]
    fn prefers_policy_helper_path_over_sibling_helper() {
        let current_exe = PathBuf::from("/usr/bin/routelane");
        let result = resolve_helper_path_from(None, Some(current_exe), |path| {
            matches!(
                path.to_str(),
                Some("/usr/bin/routelane-helper") | Some("/usr/lib/routelane/routelane-helper")
            )
        });

        assert_eq!(result, "/usr/lib/routelane/routelane-helper");
    }

    #[test]
    fn prefers_policy_helper_path_over_env_helper() {
        let env_path = PathBuf::from("/tmp/routelane-helper");
        let result = resolve_helper_path_from(Some(env_path), None, |path| {
            matches!(
                path.to_str(),
                Some("/tmp/routelane-helper") | Some("/usr/lib/routelane/routelane-helper")
            )
        });

        assert_eq!(result, "/usr/lib/routelane/routelane-helper");
    }

    #[test]
    fn keeps_policy_helper_path_literal_when_it_exists() {
        let result = resolve_helper_path_from(None, None, |path| {
            path == Path::new("/usr/lib/routelane/routelane-helper")
        });

        assert_eq!(result, "/usr/lib/routelane/routelane-helper");
    }
}
