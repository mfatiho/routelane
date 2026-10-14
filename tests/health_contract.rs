use std::fs;
use std::path::Path;

use routelane::backend::{
    KernelPlan, KernelSnapshot, LaneAvailability, LanePlan, RouteMode, SnapshotInput,
    TopologySnapshot,
};
use routelane::domain::{HealthState, LaneFailurePolicy, LaneId, ProfileId};
use routelane::health::{
    aggregate_health, evaluate_health, kernel_resources_match_plan, DiagnosticCode,
    DiagnosticSubject, Severity,
};

fn read(path: impl AsRef<Path>) -> Vec<u8> {
    fs::read(path).unwrap()
}

fn fixture(path: &str, name: &str) -> Vec<u8> {
    read(Path::new(path).join(name))
}

fn fixture_plan(path: &str) -> KernelPlan {
    serde_json::from_slice(&read(path)).unwrap()
}

fn fixture_snapshot(path: &str, plan: &KernelPlan) -> KernelSnapshot {
    let ipv4_routes = fixture(path, "ipv4-routes.json");
    let ipv6_routes = fixture(path, "ipv6-routes.json");
    let ipv4_rules = fixture(path, "ipv4-rules.json");
    let ipv6_rules = fixture(path, "ipv6-rules.json");
    let nft = fixture(path, "nft.json");
    let topology: TopologySnapshot =
        serde_json::from_slice(&fixture(path, "topology.json")).unwrap();

    KernelSnapshot::parse(SnapshotInput {
        ipv4_routes: &ipv4_routes,
        ipv6_routes: &ipv6_routes,
        ipv4_rules: &ipv4_rules,
        ipv6_rules: &ipv6_rules,
        nft: &nft,
    })
    .unwrap()
    .with_topology(&topology)
    .with_nft_ownership(plan.generation, plan.content_hash())
}

fn fixture_snapshot_with<'a>(
    path: &str,
    plan: &KernelPlan,
    ipv4_routes: &'a [u8],
    ipv6_rules: &'a [u8],
) -> KernelSnapshot {
    let ipv6_routes = fixture(path, "ipv6-routes.json");
    let ipv4_rules = fixture(path, "ipv4-rules.json");
    let nft = fixture(path, "nft.json");
    let topology: TopologySnapshot =
        serde_json::from_slice(&fixture(path, "topology.json")).unwrap();
    KernelSnapshot::parse(SnapshotInput {
        ipv4_routes,
        ipv6_routes: &ipv6_routes,
        ipv4_rules: &ipv4_rules,
        ipv6_rules,
        nft: &nft,
    })
    .unwrap()
    .with_topology(&topology)
    .with_nft_ownership(plan.generation, plan.content_hash())
}

#[test]
fn reports_healthy_when_table_rules_routes_and_interfaces_match() {
    let path = "tests/fixtures/health/healthy";
    let plan = fixture_plan(&format!("{path}/plan.json"));
    let snapshot = fixture_snapshot(path, &plan);

    let report = evaluate_health(Some(&plan), &snapshot);

    assert_eq!(report.state, HealthState::Healthy);
    assert!(report.diagnostics.is_empty());
    assert!(report
        .lanes
        .values()
        .all(|state| *state == HealthState::Healthy));
}

fn gatewayless_plan() -> KernelPlan {
    KernelPlan {
        primary: Default::default(),
        local_networks: vec![],
        generation: 9,
        profile_id: ProfileId::from("wireguard"),
        lanes: vec![LanePlan {
            identity: None,
            source_v4: vec![],
            source_v6: vec![],
            id: LaneId::from("work"),
            interface: "wg0".to_owned(),
            route_v4: Some(RouteMode::Device),
            route_v6: None,
            availability: LaneAvailability::Available,
            table: 20_000,
            mark: 0x524c0001,
            rule_priority: 10_000,
            failure_policy: LaneFailurePolicy::Inactive,
        }],
        rules: vec![],
    }
}

#[test]
fn gatewayless_default_device_route_is_healthy_without_gateway_diagnostic() {
    let plan = gatewayless_plan();
    let snapshot = KernelSnapshot::parse(SnapshotInput {
        ipv4_routes: br#"[
            {"dst":"default","dev":"wg0","table":20000,"scope":"link","flags":[]},
            {"dst":"default","gateway":"10.0.0.1","dev":"wg0","table":254,"flags":[]}
        ]"#,
        ipv6_routes: b"[]",
        ipv4_rules: br#"[{"priority":10000,"src":"all","fwmark":"0x524c0001","table":20000}]"#,
        ipv6_rules: b"[]",
        nft: br#"{"nftables":[{"table":{"family":"inet","name":"routelane"}}]}"#,
    })
    .unwrap()
    .with_nft_ownership(plan.generation, plan.content_hash());

    let report = evaluate_health(Some(&plan), &snapshot);

    assert_eq!(report.state, HealthState::Healthy);
    assert!(report.diagnostics.is_empty());
}

#[test]
fn missing_managed_gatewayless_route_is_degraded_not_gateway_unreachable() {
    let plan = gatewayless_plan();
    let mut topology = TopologySnapshot::empty();
    topology.insert_routes("wg0", Some(RouteMode::Device), None);
    let snapshot = KernelSnapshot::parse(SnapshotInput {
        ipv4_routes: b"[]",
        ipv6_routes: b"[]",
        ipv4_rules: br#"[{"priority":10000,"src":"all","fwmark":"0x524c0001","table":20000}]"#,
        ipv6_rules: b"[]",
        nft: br#"{"nftables":[{"table":{"family":"inet","name":"routelane"}}]}"#,
    })
    .unwrap()
    .with_topology(&topology)
    .with_nft_ownership(plan.generation, plan.content_hash());

    let report = evaluate_health(Some(&plan), &snapshot);

    assert_eq!(report.state, HealthState::Degraded);
    assert!(report
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == DiagnosticCode::RoutingResourceConflict));
    assert!(report
        .diagnostics
        .iter()
        .all(|diagnostic| diagnostic.code != DiagnosticCode::LaneGatewayUnreachable));
}

#[test]
fn reports_missing_lane_without_collapsing_other_lane_health() {
    let path = "tests/fixtures/health/missing-lane";
    let plan = fixture_plan(&format!("{path}/plan.json"));
    let snapshot = fixture_snapshot(path, &plan);

    let report = evaluate_health(Some(&plan), &snapshot);

    assert_eq!(report.state, HealthState::Degraded);
    assert_eq!(
        report.diagnostics[0].code,
        DiagnosticCode::LaneInterfaceMissing
    );
    assert_eq!(report.diagnostics[0].severity, Severity::Warning);
    assert_eq!(
        report.diagnostics[0].subject,
        DiagnosticSubject::Lane(LaneId::from("work"))
    );
    assert!(report.diagnostics[0]
        .suggested_action
        .as_ref()
        .is_some_and(|action| !action.description.is_empty()));
    assert_eq!(report.lanes[&LaneId::from("work")], HealthState::Degraded);
    assert_eq!(report.lanes[&LaneId::from("direct")], HealthState::Healthy);
}

#[test]
fn diagnostic_ipc_shape_uses_typed_severity_subject_and_optional_action() {
    let path = "tests/fixtures/health/missing-lane";
    let plan = fixture_plan(&format!("{path}/plan.json"));
    let snapshot = fixture_snapshot(path, &plan);
    let diagnostic = &evaluate_health(Some(&plan), &snapshot).diagnostics[0];

    let json = serde_json::to_value(diagnostic).unwrap();

    assert_eq!(json["severity"], "Warning");
    assert_eq!(
        json["subject"],
        serde_json::json!({"kind": "Lane", "value": "work"})
    );
    assert!(json["suggested_action"]["description"].is_string());
}

#[test]
fn reports_inactive_without_a_plan_even_when_kernel_resources_exist() {
    let path = "tests/fixtures/health/healthy";
    let plan = fixture_plan(&format!("{path}/plan.json"));
    let snapshot = fixture_snapshot(path, &plan);

    let report = evaluate_health(None, &snapshot);

    assert_eq!(report.state, HealthState::Inactive);
    assert!(report.lanes.is_empty());
    assert!(report.diagnostics.is_empty());
}

#[test]
fn aggregate_precedence_is_failed_authorization_degraded_applying_healthy_inactive() {
    let cases = [
        (vec![HealthState::Inactive], HealthState::Inactive),
        (
            vec![HealthState::Inactive, HealthState::Healthy],
            HealthState::Healthy,
        ),
        (
            vec![HealthState::Healthy, HealthState::Applying],
            HealthState::Applying,
        ),
        (
            vec![HealthState::Applying, HealthState::Degraded],
            HealthState::Degraded,
        ),
        (
            vec![HealthState::Degraded, HealthState::NeedsAuthorization],
            HealthState::NeedsAuthorization,
        ),
        (
            vec![HealthState::NeedsAuthorization, HealthState::Failed],
            HealthState::Failed,
        ),
    ];

    for (states, expected) in cases {
        assert_eq!(aggregate_health(states), expected);
    }
}

#[test]
fn unavailable_lane_stays_degraded_when_its_interface_later_appears() {
    let path = "tests/fixtures/health/healthy";
    let mut plan = fixture_plan(&format!("{path}/plan.json"));
    plan.lanes[1].availability = LaneAvailability::Unavailable;
    let snapshot = fixture_snapshot(path, &plan);

    let report = evaluate_health(Some(&plan), &snapshot);

    assert_eq!(report.state, HealthState::Degraded);
    assert_eq!(report.lanes[&LaneId::from("work")], HealthState::Degraded);
    assert_eq!(report.lanes[&LaneId::from("direct")], HealthState::Healthy);
    assert!(report
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == DiagnosticCode::LaneGatewayUnreachable));
    assert!(kernel_resources_match_plan(&report));
}

#[test]
fn available_lane_without_a_supported_gateway_is_degraded() {
    let path = "tests/fixtures/health/healthy";
    let mut plan = fixture_plan(&format!("{path}/plan.json"));
    plan.lanes[1].route_v4 = None;
    plan.lanes[1].route_v6 = None;
    let snapshot = fixture_snapshot(path, &plan);

    let report = evaluate_health(Some(&plan), &snapshot);

    assert_eq!(report.state, HealthState::Degraded);
    assert_eq!(report.lanes[&LaneId::from("work")], HealthState::Degraded);
    assert!(report
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == DiagnosticCode::LaneGatewayUnreachable));
}

#[test]
fn missing_ipv4_route_is_reported_even_when_ipv6_route_and_both_rules_are_exact() {
    let path = "tests/fixtures/health/healthy";
    let plan = fixture_plan(&format!("{path}/plan.json"));
    let ipv4_routes = fixture("tests/fixtures/health/missing-lane", "ipv4-routes.json");
    let ipv6_rules = fixture(path, "ipv6-rules.json");
    let snapshot = fixture_snapshot_with(path, &plan, &ipv4_routes, &ipv6_rules);

    let report = evaluate_health(Some(&plan), &snapshot);

    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == DiagnosticCode::RoutingResourceConflict
            && diagnostic
                .technical_detail
                .contains("IPv4 route is missing")
    }));
}

#[test]
fn missing_ipv6_rule_is_reported_even_when_both_routes_and_ipv4_rules_are_exact() {
    let path = "tests/fixtures/health/healthy";
    let plan = fixture_plan(&format!("{path}/plan.json"));
    let ipv4_routes = fixture(path, "ipv4-routes.json");
    let ipv6_rules = fixture("tests/fixtures/health/missing-lane", "ipv6-rules.json");
    let snapshot = fixture_snapshot_with(path, &plan, &ipv4_routes, &ipv6_rules);

    let report = evaluate_health(Some(&plan), &snapshot);

    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == DiagnosticCode::RoutingResourceConflict
            && diagnostic
                .technical_detail
                .contains("IPv6 policy rule is missing")
    }));
    assert!(!kernel_resources_match_plan(&report));
}
