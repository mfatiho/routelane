use crate::models::{HelperOp, IpFamily, RoutingRule, RuleTarget};
use crate::routing::executor::{
    detect_gateway, detect_gateway_v6, Executor, ROUTELANE_TABLE, PRIORITY_BASE,
};
use crate::routing::resolver::{resolve_domain, to_host_cidr};
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

// ──────────────────────────────────────────────────────────────────────────────
// RoutingStateManager — İki aşamalı model
//
//  KAPALI (is_active = false):
//    • Kurallar HashMap'te saklanır
//    • Kernel'a hiçbir şey yazılmaz
//    • pkexec tetiklenmez
//
//  AÇIK (is_active = true):
//    • activate_all() → routing tablosu + tüm ip rule'lar kernel'a uygulanır
//    • Yeni eklenen kural hemen kernel'a da yazılır
//    • deactivate_all() → reset_all_kernel() → temiz durum, kural listesi korunur
// ──────────────────────────────────────────────────────────────────────────────

pub struct RoutingStateManager {
    executor: Executor,
    /// Kullanıcının yapılandırdığı kurallar (kernel durumundan bağımsız)
    rules: HashMap<u64, RoutingRule>,
    next_id: u64,
    next_priority: u32,
    /// Yönlendirme şu an kernel'da aktif mi?
    is_active: bool,
    /// Kullanıcının seçtiği alt arayüz (sadece tercih, kernel'a yazılmaz)
    alt_interface: Option<String>,
}

impl RoutingStateManager {
    pub fn new() -> Self {
        Self {
            executor: Executor::new(),
            rules: HashMap::new(),
            next_id: 0,
            next_priority: PRIORITY_BASE,
            is_active: false,
            alt_interface: None,
        }
    }

    // ── Başlangıç temizliği ──────────────────────────────────────────────────

    /// Önceki çalışmadan kalan kuralları siler — pkexec yalnızca gerekirse çağrılır.
    pub async fn startup_cleanup(&self) -> Result<()> {
        if !crate::routing::executor::has_leftover_rules() {
            return Ok(());
        }
        log::info!("Startup: önceki oturumdan kalan kernel kuralları temizleniyor");
        self.reset_all_kernel().await
    }

    // ── Arayüz tercihi (kernel'a dokunmaz) ───────────────────────────────────

    pub fn set_alt_interface_preference(&mut self, interface: &str) {
        self.alt_interface = Some(interface.to_owned());
        log::info!(
            "Alt arayüz tercihi kaydedildi: {} (switch açılınca uygulanır)",
            interface
        );
    }

    pub fn clear_alt_interface_preference(&mut self) {
        self.alt_interface = None;
        log::info!("Alt arayüz tercihi temizlendi");
    }

    // ── Aktivasyon ───────────────────────────────────────────────────────────

    /// Switch ON: tüm kayıtlı kuralları kernel'a uygular.
    /// Tüm HelperOp'lar tek bir pkexec çağrısında toplu gönderilir.
    pub async fn activate_all(&mut self) -> Result<()> {
        if self.is_active {
            return Ok(());
        }

        let interface = self
            .alt_interface
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Önce bir yönlendirme ağı seçin"))?
            .to_owned();

        let families = self.required_families();

        // Routing tablosunu ve tüm kuralları tek batch'te gönder
        let (mut ops, routed_families) = self.route_ops_for_families(&interface, &families)?;

        for rule in self.rules.values() {
            for ip in &rule.resolved_ips {
                let family = cidr_family(ip)?;
                if !routed_families.contains(&family) {
                    continue;
                }
                ops.push(HelperOp::AddRule {
                    destination: ip.clone(),
                    table: ROUTELANE_TABLE,
                    priority: rule.priority,
                    family,
                });
            }
        }

        self.executor.execute_batch(&ops)?;
        self.is_active = true;
        log::info!("Yönlendirme aktif: {} kural uygulandı", self.rules.len());
        Ok(())
    }

    /// Switch OFF: kernel kurallarını temizler, kural listesini korur.
    /// Kullanıcı tekrar switch açınca aynı kurallar uygulanır.
    pub async fn deactivate_all(&mut self) -> Result<()> {
        if !self.is_active {
            return Ok(());
        }
        self.reset_all_kernel().await?;
        self.is_active = false;
        log::info!("Yönlendirme devre dışı (kural listesi korundu)");
        Ok(())
    }

    // ── Kernel sıfırlama (idempotent) ────────────────────────────────────────

    pub async fn reset_all_kernel(&self) -> Result<()> {
        self.executor.execute_batch(&[
            HelperOp::FlushRulesInRange {
                min_priority: PRIORITY_BASE,
                max_priority: crate::routing::executor::PRIORITY_MAX,
                family: IpFamily::V4,
            },
            HelperOp::FlushRulesInRange {
                min_priority: PRIORITY_BASE,
                max_priority: crate::routing::executor::PRIORITY_MAX,
                family: IpFamily::V6,
            },
            HelperOp::FlushTable {
                table: ROUTELANE_TABLE,
                family: IpFamily::V4,
            },
            HelperOp::FlushTable {
                table: ROUTELANE_TABLE,
                family: IpFamily::V6,
            },
        ])
    }

    /// Tam sıfırlama: kernel + kural listesi.
    pub async fn reset_all(&mut self) -> Result<()> {
        self.reset_all_kernel().await?;
        self.rules.clear();
        self.next_id = 0;
        self.next_priority = PRIORITY_BASE;
        self.is_active = false;
        Ok(())
    }

    // ── Kural ekleme ─────────────────────────────────────────────────────────

    pub async fn add_rule(&mut self, target: RuleTarget, interface: &str) -> Result<RoutingRule> {
        let priority = self.next_priority;

        let resolved_ips = match &target {
            RuleTarget::Ip(ip) => vec![ip.clone()],
            RuleTarget::Domain(domain) => {
                let ips = resolve_domain(domain).await?;
                ips.iter().map(to_host_cidr).collect()
            }
        };

        // Sadece aktifse kernel'a yaz
        if self.is_active {
            let mut families = HashSet::new();
            for ip in &resolved_ips {
                families.insert(cidr_family(ip)?);
            }

            let (mut ops, routed_families) = self.route_ops_for_families(interface, &families)?;
            ops.extend(
                resolved_ips
                    .iter()
                    .filter_map(|ip| {
                        let family = match cidr_family(ip) {
                            Ok(family) if routed_families.contains(&family) => family,
                            Ok(_) => return None,
                            Err(e) => return Some(Err(e)),
                        };
                        Some(Ok(HelperOp::AddRule {
                            destination: ip.clone(),
                            table: ROUTELANE_TABLE,
                            priority,
                            family,
                        }))
                    })
                    .collect::<Result<Vec<_>>>()?,
            );
            self.executor.execute_batch(&ops)?;
        }

        self.next_priority += 1;
        let id = self.next_id;
        self.next_id += 1;

        let rule = RoutingRule {
            id,
            target,
            resolved_ips,
            interface: interface.to_owned(),
            priority,
        };
        self.rules.insert(id, rule.clone());
        Ok(rule)
    }

    // ── Kural silme ──────────────────────────────────────────────────────────

    pub async fn remove_rule(&mut self, id: u64) -> Result<()> {
        let rule = self
            .rules
            .remove(&id)
            .with_context(|| format!("Kural {} bulunamadı", id))?;

        if self.is_active {
            self.executor.execute_batch(&[
                HelperOp::FlushRulesInRange {
                    min_priority: rule.priority,
                    max_priority: rule.priority,
                    family: IpFamily::V4,
                },
                HelperOp::FlushRulesInRange {
                    min_priority: rule.priority,
                    max_priority: rule.priority,
                    family: IpFamily::V6,
                },
            ])?;
        }
        Ok(())
    }

    // ── Sorgular ─────────────────────────────────────────────────────────────

    // ── Periyodik IP yenileme ────────────────────────────────────────────────

    /// Domain kuralının IPs'ini yeniden çözer; yeni IP'leri kernel'a ekler (aktifse).
    /// Eski IP'ler silinmez — devam eden HTTPS bağlantılarını kesmemek için.
    /// Döndürülen liste: kernel'a eklenen YENİ IP'ler (boşsa değişiklik yok).
    pub async fn refresh_rule_ips(&mut self, id: u64) -> Result<Vec<String>> {
        let (domain, priority) = {
            let rule = self
                .rules
                .get(&id)
                .ok_or_else(|| anyhow::anyhow!("Kural {} bulunamadı", id))?;
            match &rule.target {
                RuleTarget::Domain(d) => (d.clone(), rule.priority),
                RuleTarget::Ip(_) => return Ok(vec![]),
            }
        };

        let fresh: Vec<String> = resolve_domain(&domain)
            .await?
            .into_iter()
            .map(|ip| to_host_cidr(&ip))
            .collect();

        let existing: std::collections::HashSet<&str> = self.rules[&id]
            .resolved_ips
            .iter()
            .map(String::as_str)
            .collect();

        let added: Vec<String> = fresh
            .into_iter()
            .filter(|ip| !existing.contains(ip.as_str()))
            .collect();

        if !added.is_empty() {
            if self.is_active {
                let interface = self
                    .alt_interface
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("Önce bir yönlendirme ağı seçin"))?;
                let mut families = HashSet::new();
                for ip in &added {
                    families.insert(cidr_family(ip)?);
                }

                let (mut ops, routed_families) =
                    self.route_ops_for_families(interface, &families)?;
                ops.extend(
                    added
                        .iter()
                        .filter_map(|ip| {
                            let family = match cidr_family(ip) {
                                Ok(family) if routed_families.contains(&family) => family,
                                Ok(_) => return None,
                                Err(e) => return Some(Err(e)),
                            };
                            Some(Ok(HelperOp::AddRule {
                                destination: ip.clone(),
                                table: ROUTELANE_TABLE,
                                priority,
                                family,
                            }))
                        })
                        .collect::<Result<Vec<_>>>()?,
                );
                self.executor.execute_batch(&ops)?;
                log::info!(
                    "'{}' için {} yeni IP eklendi: {:?}",
                    domain,
                    added.len(),
                    added
                );
            }
            let rule = self.rules.get_mut(&id).unwrap();
            rule.resolved_ips.extend(added.iter().cloned());
        }

        Ok(added)
    }

    /// Domain kurallarının ID listesi (IP kuralları hariç)
    pub fn domain_rule_ids(&self) -> Vec<u64> {
        self.rules
            .iter()
            .filter(|(_, r)| matches!(r.target, RuleTarget::Domain(_)))
            .map(|(id, _)| *id)
            .collect()
    }

    // ── Sorgular ─────────────────────────────────────────────────────────────

    pub fn is_active(&self) -> bool {
        self.is_active
    }

    pub fn alt_interface(&self) -> Option<&str> {
        self.alt_interface.as_deref()
    }

    pub fn rules(&self) -> impl Iterator<Item = &RoutingRule> {
        self.rules.values()
    }

    fn required_families(&self) -> HashSet<IpFamily> {
        self.rules
            .values()
            .flat_map(|rule| rule.resolved_ips.iter())
            .filter_map(|ip| cidr_family(ip).ok())
            .collect()
    }

    fn route_ops_for_families(
        &self,
        interface: &str,
        families: &HashSet<IpFamily>,
    ) -> Result<(Vec<HelperOp>, HashSet<IpFamily>)> {
        let mut ops = Vec::new();
        let mut routed_families = HashSet::new();

        if families.contains(&IpFamily::V4) {
            let gateway = detect_gateway(interface)
                .with_context(|| format!("'{}' için IPv4 gateway bulunamadı", interface))?;
            ops.push(HelperOp::AddRoute {
                gateway,
                interface: interface.to_owned(),
                table: ROUTELANE_TABLE,
                family: IpFamily::V4,
            });
            routed_families.insert(IpFamily::V4);
        }

        if families.contains(&IpFamily::V6) {
            if let Some(gateway) = detect_gateway_v6(interface) {
                ops.push(HelperOp::AddRoute {
                    gateway,
                    interface: interface.to_owned(),
                    table: ROUTELANE_TABLE,
                    family: IpFamily::V6,
                });
                routed_families.insert(IpFamily::V6);
            } else {
                log::warn!(
                    "'{}' için IPv6 gateway bulunamadı; IPv6 hedefler atlanıyor",
                    interface
                );
            }
        }

        if !families.is_empty() && routed_families.is_empty() {
            anyhow::bail!("'{}' için uygun gateway bulunamadı", interface);
        }

        Ok((ops, routed_families))
    }
}

fn cidr_family(value: &str) -> Result<IpFamily> {
    let ip = value
        .split_once('/')
        .map(|(ip, _)| ip)
        .unwrap_or(value)
        .parse::<IpAddr>()
        .with_context(|| format!("Geçersiz IP/CIDR: {}", value))?;

    Ok(match ip {
        IpAddr::V4(_) => IpFamily::V4,
        IpAddr::V6(_) => IpFamily::V6,
    })
}
