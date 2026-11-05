use routelane::domain::{
    validate_profile, ApplicationSelector, InterfaceSelector, Lane, LaneFailurePolicy, Profile,
    Rule, RuleAction, RuleSource, RuleTarget,
};

fn rule(id: &str, target: RuleTarget) -> Rule {
    Rule {
        id: id.into(),
        enabled: true,
        priority: 10,
        source: RuleSource::Any,
        target,
        action: RuleAction::Block,
    }
}

fn profile(rules: Vec<Rule>) -> Profile {
    Profile {
        id: "work".into(),
        name: "Work".into(),
        enabled: false,
        lanes: vec![],
        rules,
        routing: Default::default(),
    }
}

#[test]
fn normalized_duplicates_are_rejected_regardless_of_id_priority_or_enabled_state() {
    for (left, right) in [
        (
            RuleTarget::Cidr("10.65.201.0/24".parse().unwrap()),
            RuleTarget::Cidr("10.65.201.25/24".parse().unwrap()),
        ),
        (
            RuleTarget::Ip("10.65.201.25".parse().unwrap()),
            RuleTarget::Cidr("10.65.201.25/32".parse().unwrap()),
        ),
        (
            RuleTarget::Cidr("2001:db8::/48".parse().unwrap()),
            RuleTarget::Cidr("2001:db8::25/48".parse().unwrap()),
        ),
        (
            RuleTarget::Ip("2001:db8::25".parse().unwrap()),
            RuleTarget::Cidr("2001:db8::25/128".parse().unwrap()),
        ),
        (
            RuleTarget::Domain("Example.COM.".into()),
            RuleTarget::Domain("example.com".into()),
        ),
    ] {
        let original = rule("original", left);
        let mut duplicate = rule("duplicate", right);
        duplicate.priority = 90;
        duplicate.enabled = false;
        assert!(
            validate_profile(&profile(vec![original, duplicate])).is_err(),
            "equivalent rule targets must not be saved twice"
        );
    }
}

#[test]
fn overlapping_networks_and_different_actions_are_not_duplicates() {
    let original = rule(
        "subnet",
        RuleTarget::Cidr("10.65.201.0/24".parse().unwrap()),
    );
    let host = rule("host", RuleTarget::Ip("10.65.201.25".parse().unwrap()));
    let mut direct = original.clone();
    direct.id = "direct".into();
    direct.action = RuleAction::Direct;
    assert!(validate_profile(&profile(vec![original, host, direct])).is_ok());
}

#[test]
fn different_route_lanes_remain_distinct() {
    let mut p = profile(vec![]);
    p.lanes = ["primary", "backup"]
        .into_iter()
        .map(|id| Lane {
            id: id.into(),
            name: id.into(),
            interface: InterfaceSelector::from("eth0"),
            fallback_lane: None,
            failure_policy: LaneFailurePolicy::Block,
        })
        .collect();
    p.rules = ["primary", "backup"]
        .into_iter()
        .map(|id| {
            let mut r = rule(id, RuleTarget::Cidr("10.65.201.0/24".parse().unwrap()));
            r.action = RuleAction::Route(id.into());
            r
        })
        .collect();
    assert!(validate_profile(&p).is_ok());
}

#[test]
fn application_metadata_cannot_disguise_the_same_process_rule() {
    let mut original = rule("first", RuleTarget::Any);
    original.action = RuleAction::Route("primary".into());
    original.source = RuleSource::Application(ApplicationSelector {
        desktop_id: Some("browser".into()),
        executable_path: "/usr/bin/browser".into(),
        include_children: true,
        launch: None,
    });
    let mut duplicate = original.clone();
    duplicate.id = "second".into();
    if let RuleSource::Application(selector) = &mut duplicate.source {
        selector.desktop_id = Some("browser-alias".into());
    }
    let mut p = profile(vec![original, duplicate]);
    p.lanes.push(Lane {
        id: "primary".into(),
        name: "Primary".into(),
        interface: "eth0".into(),
        fallback_lane: None,
        failure_policy: LaneFailurePolicy::Block,
    });
    assert!(validate_profile(&p).is_err());
}
