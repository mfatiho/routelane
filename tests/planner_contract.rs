use std::net::Ipv4Addr;

use routelane::backend::{
    compile_profile, Capability, CompiledAction, CompiledTarget, LaneAvailability, PlanError,
    ResolutionSnapshot, RouteMode, TopologySnapshot,
};
use routelane::domain::{
    ApplicationSelector, DomainError, InterfaceSelector, Lane, LaneFailurePolicy, LaneId, Profile,
    ProfileId, Rule, RuleAction, RuleId, RuleSource, RuleTarget, ServicePackId,
};

fn lane(id: &str, interface: &str) -> Lane {
    Lane {
        id: LaneId::from(id),
        name: id.to_owned(),
        interface: InterfaceSelector::from(interface),
        fallback_lane: None,
        failure_policy: LaneFailurePolicy::Inactive,
    }
}

fn rule(id: &str, priority: u32, action: RuleAction) -> Rule {
    rule_with(
        id,
        priority,
        RuleSource::Any,
        RuleTarget::Ip("203.0.113.7".parse().unwrap()),
        action,
    )
}

fn rule_with(
    id: &str,
    priority: u32,
    source: RuleSource,
    target: RuleTarget,
    action: RuleAction,
) -> Rule {
    Rule {
        id: RuleId::from(id),
        enabled: true,
        priority,
        source,
        target,
        action,
    }
}

fn profile(lanes: Vec<Lane>, rules: Vec<Rule>) -> Profile {
    Profile {
        id: ProfileId::from("default"),
        name: "Default".to_owned(),
        enabled: true,
        lanes,
        rules,
    }
}

fn profile_with_three_lanes_and_unsorted_rules() -> Profile {
    profile(
        vec![
            lane("work", "wg-work"),
            lane("direct", "eth0"),
            lane("privacy", "tun-privacy"),
        ],
        vec![
            rule("priority-20", 20, RuleAction::Direct),
            rule("priority-10-b", 10, RuleAction::Block),
            rule("priority-10-a", 10, RuleAction::Direct),
        ],
    )
}

fn profile_with_block_rule() -> Profile {
    profile(vec![], vec![rule("block", 10, RuleAction::Block)])
}

fn topology_for(entries: &[(&str, &str)]) -> TopologySnapshot {
    let mut topology = TopologySnapshot::empty();
    for (interface, gateway) in entries {
        topology.insert(*interface, Some(gateway.parse::<Ipv4Addr>().unwrap()), None);
    }
    topology
}

fn profile_with_lane_count(count: usize) -> Profile {
    profile(
        (0..count)
            .map(|slot| lane(&format!("lane-{slot:03}"), &format!("tun{slot}")))
            .collect(),
        vec![],
    )
}

fn topology_for_generated_lanes(count: usize) -> TopologySnapshot {
    let mut topology = TopologySnapshot::empty();
    for slot in 0..count {
        topology.insert(
            format!("tun{slot}"),
            Some(Ipv4Addr::new(10, 0, (slot / 256) as u8, slot as u8)),
            None,
        );
    }
    topology
}

fn profile_routing_to(lane_id: &str) -> Profile {
    profile(
        vec![lane(lane_id, "wg0")],
        vec![rule(
            "route-rule",
            10,
            RuleAction::Route(LaneId::from(lane_id)),
        )],
    )
}

fn profile_with_domain_rule(domain: &str) -> Profile {
    profile(
        vec![],
        vec![rule_with(
            "domain-rule",
            10,
            RuleSource::Any,
            RuleTarget::Domain(domain.to_owned()),
            RuleAction::Direct,
        )],
    )
}

fn profile_with_unavailable_primary_and_fallback() -> Profile {
    let mut primary = lane("primary", "wg-primary");
    primary.fallback_lane = Some(LaneId::from("middle"));
    primary.failure_policy = LaneFailurePolicy::Block;
    let mut middle = lane("middle", "wg-middle");
    middle.fallback_lane = Some(LaneId::from("fallback"));

    profile(
        vec![primary, middle, lane("fallback", "wg-fallback")],
        vec![rule(
            "route-rule",
            10,
            RuleAction::Route(LaneId::from("primary")),
        )],
    )
}

fn mark_for_lane(plan: &routelane::backend::KernelPlan, lane_id: &str) -> u32 {
    plan.lanes
        .iter()
        .find(|lane| lane.id == LaneId::from(lane_id))
        .unwrap()
        .mark
}

#[test]
fn compiles_three_lanes_into_reserved_resources_and_stable_rule_order() {
    let profile = profile_with_three_lanes_and_unsorted_rules();
    let topology = topology_for(&[
        ("wg-work", "10.0.0.1"),
        ("tun-privacy", "10.8.0.1"),
        ("eth0", "192.0.2.1"),
    ]);
    let resolutions = ResolutionSnapshot::empty();

    let plan = compile_profile(&profile, &topology, &resolutions, 7).unwrap();

    assert_eq!(plan.generation, 7);
    assert_eq!(
        plan.lanes
            .iter()
            .map(|lane| (&lane.id, lane.table, lane.mark, lane.rule_priority))
            .collect::<Vec<_>>(),
        vec![
            (&LaneId::from("direct"), 20_000, 0x524c0001, 10_000),
            (&LaneId::from("privacy"), 20_001, 0x524c0002, 10_001),
            (&LaneId::from("work"), 20_002, 0x524c0003, 10_002),
        ]
    );
    assert_eq!(
        plan.rules
            .iter()
            .map(|rule| rule.id.clone())
            .collect::<Vec<_>>(),
        vec![
            RuleId::from("priority-10-a"),
            RuleId::from("priority-10-b"),
            RuleId::from("priority-20"),
        ]
    );
}

#[test]
fn ignores_disabled_block_rule_without_removing_it_from_profile() {
    let mut profile = profile_with_block_rule();
    profile.rules[0].enabled = false;

    let plan = compile_profile(
        &profile,
        &topology_for(&[]),
        &ResolutionSnapshot::empty(),
        1,
    )
    .unwrap();

    assert!(plan.rules.is_empty());
    assert!(matches!(profile.rules[0].action, RuleAction::Block));
}

#[test]
fn identical_input_produces_identical_hash() {
    let profile = profile_with_three_lanes_and_unsorted_rules();
    let topology = topology_for(&[
        ("wg-work", "10.0.0.1"),
        ("tun-privacy", "10.8.0.1"),
        ("eth0", "192.0.2.1"),
    ]);
    let resolutions = ResolutionSnapshot::empty();

    let first = compile_profile(&profile, &topology, &resolutions, 42).unwrap();
    let second = compile_profile(&profile, &topology, &resolutions, 42).unwrap();

    assert_eq!(first, second);
    assert_eq!(first.content_hash(), second.content_hash());
}

#[test]
fn content_hash_represents_policy_not_generation_counter() {
    let profile = profile_with_three_lanes_and_unsorted_rules();
    let topology = topology_for(&[
        ("wg-work", "10.0.0.1"),
        ("tun-privacy", "10.8.0.1"),
        ("eth0", "192.0.2.1"),
    ]);
    let resolutions = ResolutionSnapshot::empty();

    let first = compile_profile(&profile, &topology, &resolutions, 41).unwrap();
    let second = compile_profile(&profile, &topology, &resolutions, 42).unwrap();

    assert_ne!(first.generation, second.generation);
    assert_eq!(first.content_hash(), second.content_hash());
}

#[test]
fn content_hash_changes_when_compiled_policy_changes() {
    let first_profile = profile_with_block_rule();
    let mut second_profile = first_profile.clone();
    second_profile.rules[0].action = RuleAction::Direct;

    let first = compile_profile(
        &first_profile,
        &TopologySnapshot::empty(),
        &ResolutionSnapshot::empty(),
        42,
    )
    .unwrap();
    let second = compile_profile(
        &second_profile,
        &TopologySnapshot::empty(),
        &ResolutionSnapshot::empty(),
        42,
    )
    .unwrap();

    assert_ne!(first.content_hash(), second.content_hash());
}

#[test]
fn rejects_inactive_profile_explicitly() {
    let mut profile = profile_with_block_rule();
    profile.enabled = false;

    let error = compile_profile(
        &profile,
        &topology_for(&[]),
        &ResolutionSnapshot::empty(),
        1,
    )
    .unwrap_err();

    assert_eq!(error, PlanError::ProfileInactive);
}

#[test]
fn inactive_profile_error_precedes_domain_validation() {
    let mut profile = profile_with_lane_count(101);
    profile.enabled = false;

    let error = compile_profile(
        &profile,
        &TopologySnapshot::empty(),
        &ResolutionSnapshot::empty(),
        1,
    )
    .unwrap_err();

    assert_eq!(error, PlanError::ProfileInactive);
}

#[test]
fn rejects_lane_when_reserved_table_capacity_would_overflow() {
    let profile = profile_with_lane_count(101);
    let error = compile_profile(
        &profile,
        &topology_for_generated_lanes(101),
        &ResolutionSnapshot::empty(),
        1,
    )
    .unwrap_err();

    assert_eq!(
        error,
        PlanError::Domain(DomainError::TooManyLanes {
            actual: 101,
            maximum: 100,
        })
    );
}

#[test]
fn hundredth_lane_uses_the_end_of_each_reserved_resource_range() {
    let plan = compile_profile(
        &profile_with_lane_count(100),
        &topology_for_generated_lanes(100),
        &ResolutionSnapshot::empty(),
        1,
    )
    .unwrap();

    let last_lane = plan.lanes.last().unwrap();
    assert_eq!(
        (last_lane.table, last_lane.mark, last_lane.rule_priority,),
        (20_099, 0x524c0064, 10_099)
    );
}

#[test]
fn route_rule_uses_the_mark_assigned_to_its_lane() {
    let plan = compile_profile(
        &profile_routing_to("work"),
        &topology_for(&[("wg0", "10.0.0.1")]),
        &ResolutionSnapshot::empty(),
        1,
    )
    .unwrap();

    assert_eq!(plan.rules[0].action, CompiledAction::Mark(0x524c0001));
}

#[test]
fn gatewayless_default_device_route_makes_wireguard_lane_available() {
    let mut topology = TopologySnapshot::empty();
    topology.insert_routes("wg0", Some(RouteMode::Device), Some(RouteMode::Device));

    let plan = compile_profile(
        &profile_routing_to("work"),
        &topology,
        &ResolutionSnapshot::empty(),
        1,
    )
    .unwrap();

    assert_eq!(plan.lanes[0].availability, LaneAvailability::Available);
    assert_eq!(plan.lanes[0].route_v4, Some(RouteMode::Device));
    assert_eq!(plan.lanes[0].route_v6, Some(RouteMode::Device));
    assert_eq!(plan.rules[0].action, CompiledAction::Mark(0x524c0001));
}

#[test]
fn interface_without_default_route_evidence_remains_unavailable() {
    let mut topology = TopologySnapshot::empty();
    topology.insert("wg0", None, None);

    let plan = compile_profile(
        &profile_routing_to("work"),
        &topology,
        &ResolutionSnapshot::empty(),
        1,
    )
    .unwrap();

    assert_eq!(plan.lanes[0].availability, LaneAvailability::Unavailable);
    assert!(plan.rules.is_empty());
}

#[test]
fn exact_domain_uses_only_addresses_from_resolution_snapshot() {
    let profile = profile_with_domain_rule("api.example.com");
    let resolutions = ResolutionSnapshot::from_rule(
        RuleId::from("domain-rule"),
        vec!["203.0.113.8/32".parse().unwrap()],
    );

    let plan = compile_profile(&profile, &topology_for(&[]), &resolutions, 1).unwrap();

    assert_eq!(
        plan.rules[0].target,
        CompiledTarget::Network("203.0.113.8/32".parse().unwrap())
    );
}

#[test]
fn unavailable_lane_uses_first_available_fallback_then_original_failure_policy() {
    let profile = profile_with_unavailable_primary_and_fallback();
    let topology = topology_for(&[("wg-fallback", "10.8.0.1")]);

    let with_fallback =
        compile_profile(&profile, &topology, &ResolutionSnapshot::empty(), 1).unwrap();
    assert_eq!(
        with_fallback.rules[0].action,
        CompiledAction::Mark(mark_for_lane(&with_fallback, "fallback"))
    );

    let without_fallback = compile_profile(
        &profile,
        &topology_for(&[]),
        &ResolutionSnapshot::empty(),
        2,
    )
    .unwrap();
    assert_eq!(without_fallback.rules[0].action, CompiledAction::Block);
}

#[test]
fn lane_slots_stay_stable_when_topology_changes() {
    let profile = profile_with_three_lanes_and_unsorted_rules();
    let complete = compile_profile(
        &profile,
        &topology_for(&[
            ("wg-work", "10.0.0.1"),
            ("tun-privacy", "10.8.0.1"),
            ("eth0", "192.0.2.1"),
        ]),
        &ResolutionSnapshot::empty(),
        1,
    )
    .unwrap();
    let missing_privacy = compile_profile(
        &profile,
        &topology_for(&[("wg-work", "10.0.0.1"), ("eth0", "192.0.2.1")]),
        &ResolutionSnapshot::empty(),
        2,
    )
    .unwrap();

    assert_eq!(
        complete
            .lanes
            .iter()
            .map(|lane| (&lane.id, lane.table, lane.mark, lane.rule_priority))
            .collect::<Vec<_>>(),
        missing_privacy
            .lanes
            .iter()
            .map(|lane| (&lane.id, lane.table, lane.mark, lane.rule_priority))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        missing_privacy.lanes[1].availability,
        LaneAvailability::Unavailable
    );
}

#[test]
fn missing_exact_domain_resolution_is_explicit() {
    let error = compile_profile(
        &profile_with_domain_rule("api.example.com"),
        &TopologySnapshot::empty(),
        &ResolutionSnapshot::empty(),
        1,
    )
    .unwrap_err();

    assert_eq!(
        error,
        PlanError::DomainResolutionUnavailable(RuleId::from("domain-rule"))
    );
}

#[test]
fn empty_exact_domain_resolution_temporarily_emits_no_rules() {
    let plan = compile_profile(
        &profile_with_domain_rule("api.example.com"),
        &TopologySnapshot::empty(),
        &ResolutionSnapshot::from_rule(RuleId::from("domain-rule"), vec![]),
        1,
    )
    .unwrap();

    assert!(plan.rules.is_empty());
}

#[test]
fn enabled_application_source_is_an_explicit_unsupported_capability() {
    let application = ApplicationSelector {
        desktop_id: Some("org.example.App".to_owned()),
        executable_path: "/usr/bin/example".into(),
        include_children: true,
    };
    let profile = profile(
        vec![],
        vec![rule_with(
            "application-rule",
            10,
            RuleSource::Application(application),
            RuleTarget::Any,
            RuleAction::Direct,
        )],
    );

    assert_eq!(
        compile_profile(
            &profile,
            &TopologySnapshot::empty(),
            &ResolutionSnapshot::empty(),
            1,
        )
        .unwrap_err(),
        PlanError::UnsupportedCapability {
            rule_id: RuleId::from("application-rule"),
            capability: Capability::Application,
        }
    );
}

#[test]
fn enabled_wildcard_domain_is_an_explicit_unsupported_capability() {
    let profile = profile(
        vec![],
        vec![rule_with(
            "wildcard-rule",
            10,
            RuleSource::Any,
            RuleTarget::WildcardDomain("*.example.com".to_owned()),
            RuleAction::Direct,
        )],
    );

    assert_eq!(
        compile_profile(
            &profile,
            &TopologySnapshot::empty(),
            &ResolutionSnapshot::empty(),
            1,
        )
        .unwrap_err(),
        PlanError::UnsupportedCapability {
            rule_id: RuleId::from("wildcard-rule"),
            capability: Capability::WildcardDomain,
        }
    );
}

#[test]
fn enabled_service_pack_is_an_explicit_unsupported_capability() {
    let profile = profile(
        vec![],
        vec![rule_with(
            "service-pack-rule",
            10,
            RuleSource::Any,
            RuleTarget::ServicePack(ServicePackId::from("streaming")),
            RuleAction::Direct,
        )],
    );

    assert_eq!(
        compile_profile(
            &profile,
            &TopologySnapshot::empty(),
            &ResolutionSnapshot::empty(),
            1,
        )
        .unwrap_err(),
        PlanError::UnsupportedCapability {
            rule_id: RuleId::from("service-pack-rule"),
            capability: Capability::ServicePack,
        }
    );
}

#[test]
fn disabled_unsupported_rule_is_filtered_before_capability_checks() {
    let mut unsupported = rule_with(
        "disabled-unsupported",
        10,
        RuleSource::Any,
        RuleTarget::WildcardDomain("*.example.com".to_owned()),
        RuleAction::Direct,
    );
    unsupported.enabled = false;

    let plan = compile_profile(
        &profile(vec![], vec![unsupported]),
        &TopologySnapshot::empty(),
        &ResolutionSnapshot::empty(),
        1,
    )
    .unwrap();

    assert!(plan.rules.is_empty());
}

#[test]
fn cidr_target_is_preserved_as_a_network() {
    let profile = profile(
        vec![],
        vec![rule_with(
            "cidr-rule",
            10,
            RuleSource::Any,
            RuleTarget::Cidr("2001:db8::/48".parse().unwrap()),
            RuleAction::Direct,
        )],
    );

    let plan = compile_profile(
        &profile,
        &TopologySnapshot::empty(),
        &ResolutionSnapshot::empty(),
        1,
    )
    .unwrap();

    assert_eq!(
        plan.rules[0].target,
        CompiledTarget::Network("2001:db8::/48".parse().unwrap())
    );
}

#[test]
fn any_target_remains_a_kernel_wide_match() {
    let profile = profile(
        vec![],
        vec![rule_with(
            "any-rule",
            10,
            RuleSource::Any,
            RuleTarget::Any,
            RuleAction::Block,
        )],
    );

    let plan = compile_profile(
        &profile,
        &TopologySnapshot::empty(),
        &ResolutionSnapshot::empty(),
        1,
    )
    .unwrap();

    assert_eq!(plan.rules[0].target, CompiledTarget::Any);
}

#[test]
fn unavailable_lane_applies_direct_or_inactive_failure_policy() {
    let mut direct_lane = lane("direct-failure", "wg-direct");
    direct_lane.failure_policy = LaneFailurePolicy::Direct;
    let mut inactive_lane = lane("inactive-failure", "wg-inactive");
    inactive_lane.failure_policy = LaneFailurePolicy::Inactive;
    let profile = profile(
        vec![direct_lane, inactive_lane],
        vec![
            rule(
                "direct-rule",
                10,
                RuleAction::Route(LaneId::from("direct-failure")),
            ),
            rule(
                "inactive-rule",
                20,
                RuleAction::Route(LaneId::from("inactive-failure")),
            ),
        ],
    );

    let plan = compile_profile(
        &profile,
        &TopologySnapshot::empty(),
        &ResolutionSnapshot::empty(),
        1,
    )
    .unwrap();

    assert_eq!(plan.rules.len(), 1);
    assert_eq!(plan.rules[0].id, RuleId::from("direct-rule"));
    assert_eq!(plan.rules[0].action, CompiledAction::Direct);
}
