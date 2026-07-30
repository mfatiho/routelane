use std::net::IpAddr;

use anyhow::{Context, Result};

/// Resolves a domain using the host resolver.
///
/// The caller is responsible for periodic refresh and for treating the result
/// as a point-in-time snapshot; CDN answers can rotate between resolutions.
pub async fn resolve_domain(domain: &str) -> Result<Vec<IpAddr>> {
    let domain = domain.to_owned();
    tokio::task::spawn_blocking(move || {
        use std::net::ToSocketAddrs;
        let addresses = format!("{domain}:80")
            .to_socket_addrs()
            .with_context(|| format!("DNS resolution failed for '{domain}'"))?
            .map(|address| address.ip())
            .collect::<Vec<_>>();
        if addresses.is_empty() {
            anyhow::bail!("DNS resolution returned no addresses for '{domain}'");
        }
        Ok(addresses)
    })
    .await
    .context("DNS resolution task panicked")?
}

pub fn to_host_cidr(ip: &IpAddr) -> String {
    match ip {
        IpAddr::V4(value) => format!("{value}/32"),
        IpAddr::V6(value) => format!("{value}/128"),
    }
}
