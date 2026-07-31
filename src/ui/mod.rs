pub mod experience;
pub mod i18n;
pub mod rule_row;
pub mod settings;
pub mod tray;
pub mod window;

#[cfg(test)]
mod experience_contract_tests {
    use super::experience::{
        activation_action, dashboard_state, ActivationAction, AuthorizationPresentation,
        PrimaryAction, SetupStep,
    };
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
}
