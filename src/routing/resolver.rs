use anyhow::{Context, Result};
use std::net::IpAddr;

/// Bir alan adını IP adreslerine çözer (senkron, spawn_blocking içinde çalıştırılır).
///
/// ÖNEMLİ SINIRLAMALAR:
/// - Çözümleme işletim sisteminin /etc/resolv.conf'unu kullanır.
/// - CDN'li domainler (chatgpt.com → Cloudflare, google.com → Anycast)
///   yüzlerce IP rotasyonu yapabilir. Belirli bir çözümleme anında dönen
///   IP seti dakikalar içinde değişebilir. Kesin yönlendirme garantisi yoktur.
/// - Güvenilir domain yönlendirmesi için dnsmasq + nftables ipset yaklaşımı önerilir.
///
/// Güvenilir kullanım senaryoları:
/// - Statik/değişmeyen IP'ler veya CIDR aralıkları (bkz. RuleTarget::Ip)
/// - Kurumsal ağ adresleri (özel IP aralıkları, sabit sunucular)
pub async fn resolve_domain(domain: &str) -> Result<Vec<IpAddr>> {
    let domain = domain.to_owned();

    tokio::task::spawn_blocking(move || {
        use std::net::ToSocketAddrs;
        // DNS çözümlemesi için bir port eklememiz gerekiyor
        let addrs: Vec<IpAddr> = format!("{}:80", domain)
            .to_socket_addrs()
            .with_context(|| format!("'{}' DNS çözümlemesi başarısız", domain))?
            .map(|sa| sa.ip())
            .collect();

        if addrs.is_empty() {
            anyhow::bail!("'{}' için hiçbir IP adresi bulunamadı", domain);
        }
        Ok(addrs)
    })
    .await
    .context("DNS çözümleme görevi panikle sonuçlandı")?
}

/// IP adresini CIDR formatına dönüştürür (/32 veya /128)
pub fn to_host_cidr(ip: &IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => format!("{}/32", v4),
        IpAddr::V6(v6) => format!("{}/128", v6),
    }
}
