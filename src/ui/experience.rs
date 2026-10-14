use routelane::domain::Profile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupStep {
    AddLane,
    AddRule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryAction {
    OpenConnections,
    RefreshStatus,
    OpenRuleDialog,
    ReviewDefinitions,
    EnableRouting,
    DisableRouting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationAction {
    PresentExisting,
    CreateWindow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DashboardState {
    pub setup_step: Option<SetupStep>,
    pub primary_action: PrimaryAction,
    pub primary_action_enabled: bool,
}

pub fn activation_action(has_existing_window: bool) -> ActivationAction {
    if has_existing_window {
        ActivationAction::PresentExisting
    } else {
        ActivationAction::CreateWindow
    }
}

pub fn dashboard_state(
    profile: &Profile,
    session: &crate::ui::session::UiSession,
) -> DashboardState {
    use routelane::domain::{RuleAction, RuleTarget};
    let active = session
        .confirmed_status()
        .is_some_and(|status| status.applied_profile_id.is_some());
    let ready = profile.rules.iter().any(|rule| {
        rule.enabled
            && !matches!(
                rule.target,
                RuleTarget::WildcardDomain(_) | RuleTarget::ServicePack(_)
            )
            && match &rule.action {
                RuleAction::Route(id) => profile.lanes.iter().any(|lane| &lane.id == id),
                _ => true,
            }
    });
    let setup = if active || ready {
        None
    } else if profile.lanes.is_empty() && profile.rules.is_empty() {
        Some(SetupStep::AddLane)
    } else {
        Some(SetupStep::AddRule)
    };
    let action = if !session.snapshot_known() {
        PrimaryAction::RefreshStatus
    } else if active {
        PrimaryAction::DisableRouting
    } else {
        match setup {
            Some(SetupStep::AddLane) => PrimaryAction::OpenConnections,
            Some(SetupStep::AddRule) if !profile.rules.is_empty() => {
                PrimaryAction::ReviewDefinitions
            }
            Some(SetupStep::AddRule) => PrimaryAction::OpenRuleDialog,
            None => PrimaryAction::EnableRouting,
        }
    };
    DashboardState {
        setup_step: setup,
        primary_action: action,
        primary_action_enabled: !session.is_busy()
            && (matches!(
                action,
                PrimaryAction::DisableRouting | PrimaryAction::RefreshStatus
            ) || session.config_writable()),
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;
    use crate::models::DaemonToUi;
    use crate::ui::session::UiSession;
    use routelane::domain::{Rule, RuleAction, RuleSource, RuleTarget};
    use routelane::health::HealthReport;
    use routelane::ipc::DaemonStatus;

    fn profile() -> Profile {
        Profile {
            routing: Default::default(),
            id: "work".into(),
            name: "Work".into(),
            enabled: false,
            lanes: vec![],
            rules: vec![],
        }
    }
    fn session(profile: &Profile, active: bool, writable: bool) -> UiSession {
        let mut s = UiSession::new(profile.clone(), writable);
        s.handle_daemon(DaemonToUi::ConnectionEstablished { epoch: 1 });
        s.handle_daemon(DaemonToUi::Snapshot {
            epoch: 1,
            status: DaemonStatus {
                generation: 7,
                event_sequence: 1,
                health: HealthReport::inactive(),
                desired_profile_id: None,
                applied_profile_id: active.then(|| profile.id.clone()),
                last_error: None,
            },
            profiles: vec![],
        });
        s
    }

    #[test]
    fn direct_and_block_rules_need_no_fake_lane() {
        for action in [RuleAction::Direct, RuleAction::Block] {
            let mut p = profile();
            p.rules.push(Rule {
                id: "rule".into(),
                enabled: true,
                priority: 10,
                source: RuleSource::Any,
                target: RuleTarget::Any,
                action,
            });
            assert_eq!(
                dashboard_state(&p, &session(&p, false, true)).primary_action,
                PrimaryAction::EnableRouting
            );
        }
    }
    #[test]
    fn active_plan_keeps_cleanup_visible_despite_missing_setup_and_readonly_config() {
        let p = profile();
        let state = dashboard_state(&p, &session(&p, true, false));
        assert_eq!(state.primary_action, PrimaryAction::DisableRouting);
        assert!(state.primary_action_enabled);
    }
    #[test]
    fn disabled_and_unsupported_rules_do_not_satisfy_setup() {
        let mut p = profile();
        p.rules.push(Rule {
            id: "rule".into(),
            enabled: false,
            priority: 10,
            source: RuleSource::Any,
            target: RuleTarget::Any,
            action: RuleAction::Direct,
        });
        assert_ne!(
            dashboard_state(&p, &session(&p, false, true)).primary_action,
            PrimaryAction::EnableRouting
        );
        p.rules[0].enabled = true;
        p.rules[0].target = RuleTarget::WildcardDomain("*.example.com".into());
        assert_ne!(
            dashboard_state(&p, &session(&p, false, true)).primary_action,
            PrimaryAction::EnableRouting
        );
    }
}
