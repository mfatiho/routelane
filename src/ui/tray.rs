use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread;
use std::time::Duration;

use crate::ui::i18n::LanguageTexts;
use glib::Sender;
use ksni::blocking::TrayMethods;
use routelane::config::Language;
use routelane::domain::Profile;

use crate::ui::{
    experience::{dashboard_state, PrimaryAction},
    overview::{status_copy, traffic_rows},
    session::UiSession,
};

pub const APP_ID: &str = "io.github.routelane";
pub const APP_ICON_NAME: &str = "network-workgroup-symbolic";

#[derive(Debug, Clone, Copy)]
pub enum TrayCommand {
    ShowWindow,
    Routing(TrayRoutingAction),
    ShowProblem,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayRoutingAction {
    Enable,
    Disable,
}

/// Owned, GTK-free snapshot. Never derive an action from an old applied status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayPresentation {
    language: Language,
    status: String,
    details: Vec<String>,
    action: Option<TrayRoutingAction>,
    action_label: &'static str,
    problem: bool,
    can_quit: bool,
    exit_note: &'static str,
}

impl TrayPresentation {
    pub fn from_session(profile: &Profile, session: &UiSession, language: Language) -> Self {
        let texts = language.texts();
        let dashboard = dashboard_state(profile, session);
        let action = if session.snapshot_known() && dashboard.primary_action_enabled {
            match dashboard.primary_action {
                PrimaryAction::EnableRouting => Some(TrayRoutingAction::Enable),
                PrimaryAction::DisableRouting => Some(TrayRoutingAction::Disable),
                _ => None,
            }
        } else {
            None
        };
        let details = if session.snapshot_known() {
            traffic_rows(profile, session, texts)
                .into_iter()
                .filter(|row| {
                    matches!(
                        row.name,
                        "overview-applied-scope"
                            | "overview-ipv4"
                            | "overview-ipv6"
                            | "overview-backup"
                    )
                })
                .map(|row| {
                    if row.subtitle.is_empty() || row.name == "overview-applied-scope" {
                        row.title
                    } else {
                        format!("{} · {}", row.title, row.subtitle)
                    }
                })
                .collect()
        } else {
            vec![texts.ux.last_known.to_owned()]
        };
        let needs_authorization = session.snapshot_known()
            && !session.is_busy()
            && session.confirmed_status().is_some_and(|s| {
                s.health.state == routelane::domain::HealthState::NeedsAuthorization
                    || s.primary.phase == routelane::backend::RuntimePhase::NeedsReauthorization
            });
        let status = status_copy(session, texts).0;
        Self {
            language,
            status: if needs_authorization {
                format!("{status} · {}", texts.dashboard.authorization_required)
            } else {
                status.into()
            },
            details,
            action,
            action_label: match dashboard.primary_action {
                PrimaryAction::DisableRouting => texts.disable_routing,
                PrimaryAction::RefreshStatus => texts.ux.checking,
                _ => texts.enable_routing,
            },
            problem: session.last_error().is_some()
                || (session.snapshot_known()
                    && session.confirmed_status().is_some_and(|s| {
                        s.last_error.is_some()
                            || matches!(
                                s.health.state,
                                routelane::domain::HealthState::Failed
                                    | routelane::domain::HealthState::Degraded
                                    | routelane::domain::HealthState::NeedsAuthorization
                            )
                            || s.primary.phase
                                == routelane::backend::RuntimePhase::NeedsReauthorization
                    })),
            can_quit: !session.is_busy(),
            exit_note: texts.tray_description,
        }
    }

    /// Revalidate on the GTK thread as well: the desktop can retain an old menu.
    pub fn allows(&self, action: TrayRoutingAction) -> bool {
        self.action == Some(action)
    }
}

struct RouteLaneTray {
    sender: Sender<TrayCommand>,
    view: TrayPresentation,
}

impl ksni::Tray for RouteLaneTray {
    fn id(&self) -> String {
        APP_ID.to_owned()
    }

    fn title(&self) -> String {
        "RouteLane".to_owned()
    }

    fn icon_name(&self) -> String {
        APP_ICON_NAME.to_owned()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "RouteLane".to_owned(),
            description: format!(
                "{} · {}\n{}",
                self.view.language.texts().tray_tooltip,
                self.view.status,
                self.view.details.join("\n")
            ),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.sender.send(TrayCommand::ShowWindow);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;

        let texts = self.view.language.texts();

        let mut menu = vec![StandardItem {
            label: format!("RouteLane · {}", self.view.status).replace('_', "__"),
            enabled: false,
            icon_name: APP_ICON_NAME.to_owned(),
            ..Default::default()
        }
        .into()];
        for detail in &self.view.details {
            menu.push(
                StandardItem {
                    label: detail.replace('_', "__"),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        menu.push(MenuItem::Separator);
        menu.push(
            StandardItem {
                label: self.view.action_label.to_owned(),
                enabled: self.view.action.is_some(),
                icon_name: "network-transmit-receive-symbolic".into(),
                activate: Box::new(|tray: &mut Self| {
                    if let Some(action) = tray.view.action {
                        let _ = tray.sender.send(TrayCommand::Routing(action));
                    }
                }),
                ..Default::default()
            }
            .into(),
        );
        if self.view.problem {
            menu.push(
                StandardItem {
                    label: texts.dashboard.tray_problem.into(),
                    icon_name: "dialog-warning-symbolic".into(),
                    activate: Box::new(|tray: &mut Self| {
                        let _ = tray.sender.send(TrayCommand::ShowProblem);
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }
        menu.extend([
            StandardItem {
                label: texts.tray_show.to_owned(),
                icon_name: "window-new-symbolic".to_owned(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.sender.send(TrayCommand::ShowWindow);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: self.view.exit_note.into(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: texts.tray_quit.to_owned(),
                enabled: self.view.can_quit,
                icon_name: "application-exit-symbolic".to_owned(),
                activate: Box::new(|tray: &mut Self| {
                    if tray.view.can_quit {
                        let _ = tray.sender.send(TrayCommand::Quit);
                    }
                }),
                ..Default::default()
            }
            .into(),
        ]);
        menu
    }
}

pub fn spawn_tray(
    sender: Sender<TrayCommand>,
    initial: TrayPresentation,
    updates: mpsc::Receiver<TrayPresentation>,
) -> Arc<AtomicBool> {
    let is_available = Arc::new(AtomicBool::new(false));
    let is_available_for_thread = is_available.clone();

    thread::spawn(move || {
        let tray = RouteLaneTray {
            sender,
            view: initial,
        };
        match tray.spawn() {
            Ok(handle) => {
                is_available_for_thread.store(true, Ordering::Relaxed);
                while !handle.is_closed() {
                    match updates.recv_timeout(Duration::from_secs(1)) {
                        Ok(mut view) => {
                            // Coalesce renders without blocking the GTK thread on D-Bus.
                            for latest in updates.try_iter() {
                                view = latest;
                            }
                            handle.update(move |tray| {
                                tray.view = view;
                            });
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            handle.shutdown().wait();
                            break;
                        }
                    }
                }
                is_available_for_thread.store(false, Ordering::Relaxed);
            }
            Err(err) => {
                log::warn!("Tray icon başlatılamadı: {}", err);
            }
        }
    });

    is_available
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{i18n::LanguageTexts, overview::tests::applied_session, session::UiSession};
    use routelane::domain::Profile;

    fn profile() -> Profile {
        Profile {
            id: "work".into(),
            name: "Work".into(),
            enabled: true,
            routing: Default::default(),
            lanes: vec![],
            rules: vec![],
        }
    }

    #[test]
    fn unconfirmed_state_cannot_toggle_routing_or_claim_it_is_active() {
        let p = profile();
        let s = UiSession::new(p.clone(), true);
        let view = TrayPresentation::from_session(&p, &s, Language::En);
        assert_eq!(view.action, None);
        assert_ne!(view.status, Language::En.texts().routing_active);
        assert_eq!(view.details, [Language::En.texts().ux.last_known]);
    }

    #[test]
    fn confirmed_active_state_offers_only_deactivation_and_gui_exit_is_explicit() {
        let p = profile();
        let s = applied_session(&p);
        let view = TrayPresentation::from_session(&p, &s, Language::En);
        assert_eq!(view.action, Some(TrayRoutingAction::Disable));
        assert_eq!(view.status, Language::En.texts().routing_active);
        assert!(view.exit_note.contains("continues"));
    }

    #[test]
    fn busy_menu_disables_mutations_and_exit() {
        let p = profile();
        let mut s = applied_session(&p);
        s.begin_request(routelane::ipc::DaemonRequest::Deactivate, None)
            .unwrap();
        let view = TrayPresentation::from_session(&p, &s, Language::Tr);
        assert_eq!(view.action, None);
        assert!(!view.can_quit);
        use ksni::Tray;
        let (sender, _receiver) = glib::MainContext::channel(glib::PRIORITY_DEFAULT);
        let tray = RouteLaneTray { sender, view };
        let menu = tray.menu();
        for label in [
            Language::Tr.texts().disable_routing,
            Language::Tr.texts().tray_quit,
        ] {
            let item = menu
                .iter()
                .find_map(|item| match item {
                    ksni::MenuItem::Standard(item) if item.label == label => Some(item),
                    _ => None,
                })
                .expect("native menu contains the action");
            assert!(!item.enabled);
        }
    }

    #[test]
    fn disconnected_daemon_never_keeps_an_active_menu_or_action() {
        let p = profile();
        let mut s = applied_session(&p);
        s.handle_daemon(crate::models::DaemonToUi::DaemonUnavailable {
            epoch: 1,
            request_id: None,
            error: routelane::ipc::IpcError::new("OFFLINE", "offline"),
        });
        assert!(
            s.confirmed_status().unwrap().applied_profile_id.is_some(),
            "fixture retains stale data"
        );
        let view = TrayPresentation::from_session(&p, &s, Language::En);
        assert_eq!(view.status, Language::En.texts().ux.unknown);
        assert_eq!(view.action, None);
        assert!(!view.allows(TrayRoutingAction::Disable));
        assert!(view.problem);
        assert_eq!(view.details, [Language::En.texts().ux.last_known]);
    }

    #[test]
    fn saved_scope_never_replaces_the_actual_applied_scope_in_the_menu() {
        let mut p = profile();
        let s = applied_session(&p);
        p.routing.unmatched_traffic = routelane::domain::UnmatchedTraffic::UsePrimary;
        for language in [Language::Tr, Language::En] {
            let view = TrayPresentation::from_session(&p, &s, language);
            let line = view
                .details
                .iter()
                .find(|line| line.contains(language.texts().primary.applied_scope))
                .unwrap();
            assert!(line.contains(language.texts().primary.scope_selective));
            assert!(!line.contains(language.texts().primary.scope_use_primary));
        }
    }

    #[test]
    fn reauthorization_is_visible_without_claiming_the_applied_plan_disappeared() {
        let p = profile();
        let mut s = applied_session(&p);
        let mut status = s.confirmed_status().unwrap().clone();
        status.event_sequence += 1;
        status.primary.phase = routelane::backend::RuntimePhase::NeedsReauthorization;
        s.handle_daemon(crate::models::DaemonToUi::Snapshot {
            epoch: 1,
            status,
            profiles: vec![p.clone()],
        });
        for language in [Language::Tr, Language::En] {
            let view = TrayPresentation::from_session(&p, &s, language);
            assert!(view.status.contains(language.texts().routing_active));
            assert!(view
                .status
                .contains(language.texts().dashboard.authorization_required));
            assert!(view.problem);
            assert_eq!(view.action, Some(TrayRoutingAction::Disable));
        }
    }
}
