pub mod application_row;
pub mod confirm;
pub mod experience;
pub mod i18n;
pub mod rule_row;
pub mod settings;
pub mod tray;
pub mod window;

#[cfg(test)]
mod experience_contract_tests {
    use super::experience::{
        activation_action, authorization_after_applied, authorization_after_error, dashboard_state,
        status_presentation, ActivationAction, AuthorizationPresentation, PrimaryAction, SetupStep,
    };
    use crate::ui::i18n::LanguageTexts;
    use routelane::config::Language;
    use routelane::domain::{
        HealthState, InterfaceSelector, Lane, LaneFailurePolicy, LaneId, Profile, ProfileId, Rule,
        RuleAction, RuleId, RuleSource, RuleTarget,
    };

    fn profile() -> Profile {
        Profile {
            id: ProfileId::from("default"),
            name: "Default".to_owned(),
            enabled: false,
            lanes: vec![],
            rules: vec![],
        }
    }

    fn profile_with_lane() -> Profile {
        let mut profile = profile();
        profile.lanes.push(Lane {
            id: LaneId::from("vpn"),
            name: "VPN".to_owned(),
            interface: InterfaceSelector::from("tun0"),
            fallback_lane: None,
            failure_policy: LaneFailurePolicy::Block,
        });
        profile
    }

    fn profile_with_lane_and_rule() -> Profile {
        let mut profile = profile_with_lane();
        profile.rules.push(Rule {
            id: RuleId::from("vpn-only"),
            enabled: true,
            priority: 100,
            source: RuleSource::Any,
            target: RuleTarget::Domain("example.com".to_owned()),
            action: RuleAction::Route(LaneId::from("vpn")),
        });
        profile
    }

    #[test]
    fn dashboard_guides_the_first_missing_setup_step() {
        assert_eq!(
            dashboard_state(&profile(), HealthState::Inactive, false).setup_step,
            Some(SetupStep::AddLane)
        );
        assert_eq!(
            dashboard_state(&profile_with_lane(), HealthState::Inactive, false).setup_step,
            Some(SetupStep::AddRule)
        );
        assert_eq!(
            dashboard_state(&profile_with_lane_and_rule(), HealthState::Inactive, false)
                .primary_action,
            PrimaryAction::EnableRouting
        );
    }

    #[test]
    fn pending_mutation_waits_for_authorization_without_a_repeat_action() {
        let state = dashboard_state(&profile_with_lane_and_rule(), HealthState::Inactive, true);

        assert_eq!(state.authorization, AuthorizationPresentation::Waiting);
        assert!(!state.primary_action_enabled);
    }

    #[test]
    fn activation_reuses_an_existing_window() {
        assert_eq!(activation_action(true), ActivationAction::PresentExisting);
        assert_eq!(activation_action(false), ActivationAction::CreateWindow);
    }

    #[test]
    fn every_language_has_actionable_setup_and_authorization_copy() {
        for texts in [Language::Tr.texts(), Language::En.texts()] {
            assert!(!texts.enable_routing.is_empty());
            assert!(!texts.add_first_lane.is_empty());
            assert!(!texts.authorization_waiting.is_empty());
            assert!(!texts.authorization_denied.is_empty());
        }
    }

    #[test]
    fn authorization_denial_reenables_control_without_changing_confirmed_state() {
        let state = authorization_after_error(true, true);

        assert_eq!(state.authorization, AuthorizationPresentation::Denied);
        assert!(state.primary_action_enabled);
        assert!(state.routing_active);
    }

    #[test]
    fn accepted_apply_clears_authorization_waiting_state() {
        assert_eq!(
            authorization_after_applied(),
            AuthorizationPresentation::Idle
        );
    }

    #[test]
    fn status_presentation_replaces_raw_health_states_with_actionable_copy() {
        for health in [
            HealthState::Inactive,
            HealthState::Applying,
            HealthState::NeedsAuthorization,
            HealthState::Healthy,
            HealthState::Degraded,
            HealthState::Failed,
        ] {
            let presentation = status_presentation(
                health,
                AuthorizationPresentation::Idle,
                Language::Tr.texts(),
            );
            assert!(!presentation.title.is_empty());
            assert!(!presentation.detail.is_empty());
            assert!(!presentation.icon_name.is_empty());
        }
    }
}
