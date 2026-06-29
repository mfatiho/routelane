use serde::{Deserialize, Serialize};
use std::net::IpAddr;

// ──────────────────────────────────────────────────────────────────────────────
// Ağ arayüzü
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkInterface {
    pub name: String,
    pub gateway: Option<String>,
    /// Sistemin varsayılan gateway'ini sağlayan arayüz
    pub is_default: bool,
}

// ──────────────────────────────────────────────────────────────────────────────
// Kural hedefi
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RuleTarget {
    /// "93.184.216.34" veya "10.0.0.0/8" gibi doğrudan IP/CIDR
    Ip(String),
    /// "chatgpt.com" gibi alan adı — kernel IP üzerinden yönlendirir;
    /// CDN'li domainlerde (Cloudflare, Fastly) dönen IP seti sürekli değişir.
    /// Periyodik yeniden çözümleme yapılır ama %100 güvenilir değildir.
    Domain(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IpFamily {
    V4,
    V6,
}

impl RuleTarget {
    pub fn from_user_input(input: &str) -> Option<Self> {
        let input = input.trim();
        if input.is_empty() {
            return None;
        }

        if let Some((addr, prefix)) = input.split_once('/') {
            if let Ok(ip) = addr.parse::<IpAddr>() {
                if prefix.parse::<u8>().ok().is_some_and(|p| {
                    matches!(ip, IpAddr::V4(_)) && p <= 32
                        || matches!(ip, IpAddr::V6(_)) && p <= 128
                }) {
                    return Some(Self::Ip(input.to_owned()));
                }
            }
        }

        if input.parse::<IpAddr>().is_ok() {
            return Some(Self::Ip(input.to_owned()));
        }

        let without_scheme = input
            .find("://")
            .map(|idx| &input[idx + 3..])
            .unwrap_or(input);

        let authority = without_scheme
            .split(['/', '?', '#'])
            .next()
            .unwrap_or(without_scheme)
            .trim();

        let authority = authority
            .rsplit_once('@')
            .map(|(_, host)| host)
            .unwrap_or(authority);

        let host = if let Some(rest) = authority.strip_prefix('[') {
            rest.split_once(']').map(|(host, _)| host).unwrap_or(rest)
        } else if let Some((host, _port)) = authority.rsplit_once(':') {
            if host.contains(':') {
                authority
            } else {
                host
            }
        } else {
            authority
        };

        let host = host.trim_matches('.').to_ascii_lowercase();
        (!host.is_empty()).then_some(Self::Domain(host))
    }

    pub fn display(&self) -> &str {
        match self {
            RuleTarget::Ip(s) | RuleTarget::Domain(s) => s.as_str(),
        }
    }
    #[allow(dead_code)]
    pub fn is_domain(&self) -> bool {
        matches!(self, RuleTarget::Domain(_))
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Yönlendirme kuralı
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRule {
    /// Uygulama içi benzersiz kimlik
    pub id: u64,
    pub target: RuleTarget,
    /// Şu an çekirdekte aktif olan IP adresleri (domain → çözümlenmiş IPs)
    pub resolved_ips: Vec<String>,
    /// Yönlendirilecek ağ arayüzü
    pub interface: String,
    /// `ip rule` önceliği — ROUTELANE_PRIORITY_BASE..=ROUTELANE_PRIORITY_MAX aralığı
    pub priority: u32,
}

#[derive(Debug, Clone)]
pub struct RouteProbe {
    pub destination: String,
    pub family: IpFamily,
    pub interface: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RouteTestReport {
    pub expected_interface: Option<String>,
    pub probes: Vec<RouteProbe>,
}

impl RoutingRule {
    pub fn subtitle(&self) -> String {
        match &self.target {
            RuleTarget::Domain(_) => self.resolved_ips.join(", "),
            RuleTarget::Ip(ip) => ip.clone(),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Mesaj kanalları
// ──────────────────────────────────────────────────────────────────────────────

/// Arayüzden yönlendirme motoruna giden mesajlar
#[derive(Debug)]
pub enum UiToEngine {
    /// Kullanıcı arayüz tercihini belirledi — kernel'a dokunulmaz
    SetAltInterface { interface: String },
    /// Kullanıcının arayüz tercihini temizle — kernel'a dokunulmaz
    ClearAltInterface,
    /// Ağ arayüzü listesini yeniden oku
    RefreshInterfaces,
    /// Yeni kural ekle (kernel'a yazılıp yazılmayacağı aktif duruma göre belirlenir)
    AddRule {
        target: RuleTarget,
        interface: String,
    },
    /// Var olan kuralı sil
    RemoveRule(u64),
    /// Switch ON/OFF — aktif=true ise tüm kurallar kernel'a uygulanır
    SetActive(bool),
    /// Tüm kuralları ve kernel durumunu sıfırla
    Reset,
    /// Girilen adresin kernel tarafından hangi arayüze yönlendirildiğini test et
    TestRoute { input: String },
    /// Uygulama kapanıyor
    Shutdown,
}

/// Yönlendirme motorundan arayüze gelen mesajlar
#[derive(Debug, Clone)]
pub enum EngineToUi {
    InterfacesList(Vec<NetworkInterface>),
    RuleAdded(RoutingRule),
    RuleRemoved(u64),
    ResetComplete,
    Deactivated,
    Error(String),
    StatusUpdate(String),
    /// Switch ON denenirken hata oluştu — UI switch'i geri almalı
    ActivationFailed(String),
    /// Kaydedilmiş ayarlar yüklendi — UI seçimleri geri yüklesin
    StateRestored {
        alt_interface: Option<String>,
    },
    RouteTestComplete(RouteTestReport),
}

// ──────────────────────────────────────────────────────────────────────────────
// routelane-helper IPC yapıları
// ──────────────────────────────────────────────────────────────────────────────

/// pkexec ile çalışan ayrıcalıklı yardımcıya gönderilen komutlar
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HelperOp {
    // ── Klasik ip-rule backend ─────────────────────────────────────────────
    /// ip route add default via <gw> dev <iface> table <table>
    AddRoute {
        gateway: String,
        interface: String,
        table: u32,
        family: IpFamily,
    },
    /// ip rule add to <dst> lookup <table> priority <prio>
    AddRule {
        destination: String,
        table: u32,
        priority: u32,
        family: IpFamily,
    },
    /// Öncelik aralığındaki tüm ip rule'ları sil (idempotent)
    FlushRulesInRange {
        min_priority: u32,
        max_priority: u32,
        family: IpFamily,
    },
    /// ip route flush table <table>
    FlushTable { table: u32, family: IpFamily },

    // ── ipset + fwmark backend (dnsmasq entegrasyonu için) ─────────────────
    /// ipset create <name> hash:ip timeout <sec> family <inet|inet6>
    IpsetCreate {
        name: String,
        family: String,
        timeout_secs: u32,
    },
    /// ipset add <name> <ip>
    IpsetAdd { name: String, ip: String },
    /// ipset flush + destroy <name>
    IpsetDestroy { name: String },
    /// iptables -t mangle -A/-D <chain> -m set --match-set <set> dst -j MARK --set-mark <mark>
    IptablesMark {
        chain: String,
        action: String,
        ipset_name: String,
        mark: u32,
    },
    /// ip rule add fwmark <mark> lookup <table> priority <prio>
    AddFwmarkRule {
        mark: u32,
        table: u32,
        priority: u32,
    },
    /// ip rule del fwmark <mark>
    DelFwmarkRule { mark: u32 },
    /// /etc/dnsmasq.d/routelane-<domain>.conf dosyasına ipset direktifi yaz
    WriteDnsmasqEntry { domain: String },
    /// /etc/dnsmasq.d/routelane-<domain>.conf dosyasını sil
    RemoveDnsmasqEntry { domain: String },
    /// dnsmasq'a SIGHUP gönder (config yeniden yükle)
    ReloadDnsmasq,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HelperRequest {
    pub ops: Vec<HelperOp>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HelperResponse {
    pub success: bool,
    pub errors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::RuleTarget;

    #[test]
    fn parses_url_as_domain_host() {
        assert_eq!(
            RuleTarget::from_user_input("https://gemini.google.com/app"),
            Some(RuleTarget::Domain("gemini.google.com".to_owned()))
        );
    }

    #[test]
    fn parses_domain_port_as_domain_host() {
        assert_eq!(
            RuleTarget::from_user_input("gemini.google.com:443"),
            Some(RuleTarget::Domain("gemini.google.com".to_owned()))
        );
    }

    #[test]
    fn parses_ipv6_and_cidr_as_ip_rules() {
        assert_eq!(
            RuleTarget::from_user_input("2001:db8::1"),
            Some(RuleTarget::Ip("2001:db8::1".to_owned()))
        );
        assert_eq!(
            RuleTarget::from_user_input("2001:db8::/32"),
            Some(RuleTarget::Ip("2001:db8::/32".to_owned()))
        );
    }
}
