pub mod dns_router;
pub mod executor;
pub mod manager;
pub mod resolver;

use crate::config::{PersistedRule, PersistedState};
use crate::models::{EngineToUi, RouteTestReport, RuleTarget, UiToEngine};
use async_channel::{Receiver, Sender};
use manager::RoutingStateManager;
use std::net::IpAddr;

pub async fn engine_main(from_ui: Receiver<UiToEngine>, to_ui: Sender<EngineToUi>) {
    let mut manager = RoutingStateManager::new();

    // Arayüz listesini gönder
    let interfaces = executor::list_interfaces();
    let _ = to_ui.send(EngineToUi::InterfacesList(interfaces)).await;

    // Önceki çalışmadan kalan kernel kurallarını temizle
    if let Err(e) = manager.startup_cleanup().await {
        log::warn!("Başlangıç temizliği: {}", e);
        let _ = to_ui
            .send(EngineToUi::Error(format!("Başlangıç temizliği: {}", e)))
            .await;
    }

    // Kaydedilmiş ayarları yükle ve geri yükle
    let saved = PersistedState::load();
    if let Some(ref iface) = saved.alt_interface {
        manager.set_alt_interface_preference(iface);
    }
    for saved_rule in &saved.rules {
        let target = RuleTarget::from_user_input(&saved_rule.target_str).unwrap_or_else(|| {
            if saved_rule.is_domain {
                RuleTarget::Domain(saved_rule.target_str.clone())
            } else {
                RuleTarget::Ip(saved_rule.target_str.clone())
            }
        });
        match manager.add_rule(target, &saved_rule.interface).await {
            Ok(rule) => {
                let _ = to_ui.send(EngineToUi::RuleAdded(rule)).await;
            }
            Err(e) => {
                log::warn!(
                    "Kaydedilen kural yüklenemedi '{}': {}",
                    saved_rule.target_str,
                    e
                );
                let _ = to_ui
                    .send(EngineToUi::Error(format!(
                        "'{}' yüklenemedi: {}",
                        saved_rule.target_str, e
                    )))
                    .await;
            }
        }
    }
    let _ = to_ui
        .send(EngineToUi::StateRestored {
            alt_interface: saved.alt_interface,
        })
        .await;

    // SIGTERM / SIGINT yakalayıcı
    let to_ui_signal = to_ui.clone();
    tokio::spawn(async move {
        let ctrl_c = async {
            tokio::signal::ctrl_c().await.ok();
        };

        #[cfg(unix)]
        let sigterm = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler")
                .recv()
                .await;
        };
        #[cfg(not(unix))]
        let sigterm = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c  => log::info!("SIGINT"),
            _ = sigterm => log::info!("SIGTERM"),
        }

        if executor::has_leftover_rules() {
            let cleanup = RoutingStateManager::new();
            cleanup.reset_all_kernel().await.ok();
        }
        let _ = to_ui_signal.send(EngineToUi::ResetComplete).await;
        std::process::exit(0);
    });

    // Domain kuralları için periyodik IP yenileme (60 saniyede bir).
    // Gemini, ChatGPT gibi CDN'li servisler DNS'te farklı IP setleri döndürebilir;
    // yenileme ile yeni IP'ler kernel'a eklenir (eskiler silinmez — aktif bağlantıları korur).
    let mut refresh_interval = tokio::time::interval(std::time::Duration::from_secs(60));
    refresh_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    refresh_interval.tick().await; // ilk anlık tick'i tüket

    // ── Ana mesaj döngüsü ──────────────────────────────────────────────────
    'main: loop {
        tokio::select! {
            // Her 60 saniyede domain IP yenileme
            _ = refresh_interval.tick() => {
                if should_refresh_domain_rules(manager.is_active()) {
                    for id in manager.domain_rule_ids() {
                        if let Err(e) = manager.refresh_rule_ips(id).await {
                            log::debug!("Periyodik yenileme kural {}: {}", id, e);
                        }
                    }
                }
            }

            // UI'dan gelen komut
            recv = from_ui.recv() => {
                let msg = match recv {
                    Ok(m)  => m,
                    Err(_) => break 'main,
                };

                match msg {
                    // ── Arayüz tercihi (kernel'a dokunmaz) ─────────────────
                    UiToEngine::SetAltInterface { interface } => {
                        manager.set_alt_interface_preference(&interface);
                        save_config(&manager);
                    }

                    UiToEngine::ClearAltInterface => {
                        manager.clear_alt_interface_preference();
                        save_config(&manager);
                    }

                    UiToEngine::RefreshInterfaces => {
                        let interfaces = executor::list_interfaces();
                        let _ = to_ui.send(EngineToUi::InterfacesList(interfaces)).await;
                    }

                    // ── Switch ON ────────────────────────────────────────────
                    UiToEngine::SetActive(true) => {
                        match manager.activate_all().await {
                            Ok(()) => {
                                let count = manager.rules().count();
                                let _ = to_ui.send(EngineToUi::StatusUpdate(
                                    format!("Aktif — {} kural yönlendiriliyor", count)
                                )).await;
                            }
                            Err(e) => {
                                let _ = to_ui.send(EngineToUi::ActivationFailed(e.to_string())).await;
                            }
                        }
                    }

                    // ── Switch OFF ───────────────────────────────────────────
                    UiToEngine::SetActive(false) => {
                        match manager.deactivate_all().await {
                            Ok(()) => {
                                let _ = to_ui.send(EngineToUi::Deactivated).await;
                                let _ = to_ui.send(EngineToUi::StatusUpdate(
                                    "Devre dışı".to_owned()
                                )).await;
                            }
                            Err(e) => {
                                let _ = to_ui.send(EngineToUi::Error(
                                    format!("Devre dışı bırakma hatası: {}", e)
                                )).await;
                            }
                        }
                    }

                    // ── Kural ekleme ─────────────────────────────────────────
                    UiToEngine::AddRule { target, interface } => {
                        let display = target.display().to_owned();
                        match manager.add_rule(target, &interface).await {
                            Ok(rule) => {
                                let _ = to_ui.send(EngineToUi::RuleAdded(rule)).await;
                                save_config(&manager);
                                if manager.is_active() {
                                    let count = manager.rules().count();
                                    let _ = to_ui.send(EngineToUi::StatusUpdate(
                                        format!("Aktif — {} kural", count)
                                    )).await;
                                }
                            }
                            Err(e) => {
                                let _ = to_ui.send(EngineToUi::Error(
                                    format!("'{}' eklenemedi: {}", display, e)
                                )).await;
                            }
                        }
                    }

                    // ── Kural silme ──────────────────────────────────────────
                    UiToEngine::RemoveRule(id) => {
                        match manager.remove_rule(id).await {
                            Ok(()) => {
                                let _ = to_ui.send(EngineToUi::RuleRemoved(id)).await;
                                save_config(&manager);
                            }
                            Err(e) => {
                                let _ = to_ui.send(EngineToUi::Error(
                                    format!("Silinemedi: {}", e)
                                )).await;
                            }
                        }
                    }

                    // ── Tam sıfırlama ────────────────────────────────────────
                    UiToEngine::Reset => {
                        match manager.reset_all().await {
                            Ok(()) => {
                                let _ = to_ui.send(EngineToUi::ResetComplete).await;
                                save_config(&manager);
                            }
                            Err(e) => {
                                let _ = to_ui.send(EngineToUi::Error(
                                    format!("Sıfırlama hatası: {}", e)
                                )).await;
                            }
                        }
                    }

                    UiToEngine::TestRoute { input } => {
                        match run_route_test(&manager, input).await {
                            Ok(report) => {
                                let _ = to_ui.send(EngineToUi::RouteTestComplete(report)).await;
                            }
                            Err(e) => {
                                let _ = to_ui.send(EngineToUi::Error(
                                    format!("Test başarısız: {}", e)
                                )).await;
                            }
                        }
                    }

                    UiToEngine::Shutdown => break 'main,
                }
            }
        }
    }

    // Kapanışta kernel temizliği
    manager.reset_all_kernel().await.ok();
}

async fn run_route_test(
    manager: &RoutingStateManager,
    input: String,
) -> anyhow::Result<RouteTestReport> {
    let target = RuleTarget::from_user_input(&input)
        .ok_or_else(|| anyhow::anyhow!("Geçerli bir adres girin"))?;

    let ips: Vec<IpAddr> = match &target {
        RuleTarget::Ip(ip) => {
            let ip = ip.split_once('/').map(|(ip, _)| ip).unwrap_or(ip);
            vec![ip.parse()?]
        }
        RuleTarget::Domain(domain) => resolver::resolve_domain(domain).await?,
    };

    let mut probes = Vec::new();
    for ip in ips.into_iter().take(8) {
        probes.push(executor::probe_route(ip)?);
    }

    Ok(RouteTestReport {
        expected_interface: manager.alt_interface().map(str::to_owned),
        probes,
    })
}

fn should_refresh_domain_rules(is_active: bool) -> bool {
    !is_active
}

fn save_config(manager: &RoutingStateManager) {
    let existing = PersistedState::load();
    let state = PersistedState {
        language: existing.language,
        alt_interface: manager.alt_interface().map(str::to_owned),
        rules: manager
            .rules()
            .map(|r| PersistedRule {
                target_str: r.target.display().to_owned(),
                is_domain: r.target.is_domain(),
                interface: r.interface.clone(),
            })
            .collect(),
    };
    if let Err(e) = state.save() {
        log::warn!("Ayarlar kaydedilemedi: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::should_refresh_domain_rules;

    #[test]
    fn skips_automatic_domain_refresh_while_routing_is_active() {
        assert!(!should_refresh_domain_rules(true));
    }

    #[test]
    fn allows_automatic_domain_refresh_while_routing_is_inactive() {
        assert!(should_refresh_domain_rules(false));
    }
}
