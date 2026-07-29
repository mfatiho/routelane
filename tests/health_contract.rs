use std::fs;
use std::path::Path;

use routelane::backend::{KernelPlan, KernelSnapshot, SnapshotInput, TopologySnapshot};
use routelane::domain::{HealthState, LaneId};
use routelane::health::{aggregate_health, evaluate_health, DiagnosticCode};

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
    assert_eq!(report.lanes[&LaneId::from("work")], HealthState::Degraded);
    assert_eq!(report.lanes[&LaneId::from("direct")], HealthState::Healthy);
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
