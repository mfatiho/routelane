#[path = "support/primary.rs"]
mod fixtures;

use routelane::backend::{
    compile_profile_with_primary, CompiledAction, FamilyExit, IpFamily, KernelPlan,
    PrimarySelector, ResolutionSnapshot, TopologySnapshot,
};
use routelane::domain::{LaneFailurePolicy, Profile, Rule, RuleAction, RuleSource, RuleTarget};
use std::time::Duration;

fn add_rule(p: &mut Profile, id: &str, priority: u32, action: RuleAction) {
    p.rules.push(Rule {
        id: id.into(),
        priority,
        action,
        enabled: true,
        source: RuleSource::Any,
        target: RuleTarget::Any,
    });
}

fn compile(p: &Profile, t: &TopologySnapshot, generation: u64) -> KernelPlan {
    let d = PrimarySelector::new(p, t).update(&p.routing, t, Duration::ZERO);
    compile_profile_with_primary(p, t, &ResolutionSnapshot::empty(), &d, generation).unwrap()
}

fn action(plan: &KernelPlan, id: &str, family: IpFamily) -> Option<CompiledAction> {
    plan.rules
        .iter()
        .find(|r| r.id.as_str() == id && r.family == family)
        .map(|r| r.action.clone())
}

fn mark(plan: &KernelPlan, id: &str) -> CompiledAction {
    CompiledAction::Mark(
        plan.lanes
            .iter()
            .find(|l| l.id.as_str() == id)
            .unwrap()
            .mark,
    )
}

#[test]
fn unmatched_and_direct_use_primary_but_vinn_rule_stays_vinn() {
    let mut p = fixtures::profile();
    add_rule(&mut p, "direct", 20, RuleAction::Direct);
    add_rule(&mut p, "vinn-site", 10, RuleAction::Route("vinn".into()));
    add_rule(&mut p, "blocked", 30, RuleAction::Block);
    let plan = compile(&p, &fixtures::topology(true, true), 1);
    assert_eq!(
        plan.primary.effective.v4,
        FamilyExit::Lane("ethernet".into())
    );
    assert_eq!(
        action(&plan, "direct", IpFamily::V4),
        Some(mark(&plan, "ethernet"))
    );
    assert_eq!(
        action(&plan, "vinn-site", IpFamily::V4),
        Some(mark(&plan, "vinn"))
    );
    assert_eq!(
        action(&plan, "blocked", IpFamily::V4),
        Some(CompiledAction::Block)
    );
    assert_eq!(
        plan.rules
            .iter()
            .filter(|r| r.family == IpFamily::V4)
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        vec!["vinn-site", "direct", "blocked"]
    );
    assert_eq!(p.rules.len(), 3);
    assert_eq!(
        plan.lanes
            .iter()
            .find(|l| l.id.as_str() == "ethernet")
            .unwrap()
            .identity
            .as_ref()
            .unwrap()
            .ifindex,
        2
    );
}

#[test]
fn block_and_lane_fallback_precedence_survive_failover() {
    let mut p = fixtures::profile();
    add_rule(&mut p, "vinn-site", 10, RuleAction::Route("vinn".into()));
    add_rule(&mut p, "block", 20, RuleAction::Block);
    p.lanes[1].fallback_lane = Some("ethernet".into());
    let plan = compile(&p, &fixtures::topology(true, false), 1);
    assert_eq!(
        action(&plan, "vinn-site", IpFamily::V4),
        Some(mark(&plan, "ethernet"))
    );
    assert_eq!(
        action(&plan, "block", IpFamily::V4),
        Some(CompiledAction::Block)
    );
    let down = compile(&p, &fixtures::topology(false, false), 2);
    assert_eq!(
        action(&down, "vinn-site", IpFamily::V4),
        Some(CompiledAction::Block)
    );
    assert_eq!(down.primary.effective.v4, FamilyExit::Blocked);
}

#[test]
fn inactive_rule_falls_through_to_primary() {
    let mut p = fixtures::profile();
    p.lanes[1].failure_policy = LaneFailurePolicy::Inactive;
    add_rule(&mut p, "inactive", 10, RuleAction::Route("vinn".into()));
    let plan = compile(&p, &fixtures::topology(true, false), 1);
    assert!(plan.rules.is_empty());
    assert_eq!(
        plan.primary.effective.v4,
        FamilyExit::Lane("ethernet".into())
    );
}

#[test]
fn missing_family_is_explicitly_blocked() {
    let mut p = fixtures::profile();
    add_rule(&mut p, "direct", 10, RuleAction::Direct);
    add_rule(&mut p, "vinn-site", 20, RuleAction::Route("vinn".into()));
    let plan = compile(
        &p,
        &fixtures::topology_case(true, true, false, false, "usb0", 3),
        1,
    );
    assert_eq!(plan.primary.effective.v6, FamilyExit::Blocked);
    assert_eq!(
        action(&plan, "direct", IpFamily::V6),
        Some(CompiledAction::Block)
    );
    assert_eq!(
        action(&plan, "vinn-site", IpFamily::V6),
        Some(CompiledAction::Block)
    );
    assert_eq!(
        action(&plan, "vinn-site", IpFamily::V4),
        Some(mark(&plan, "vinn"))
    );
}

#[test]
fn hash_changes_with_primary_not_generation() {
    let p = fixtures::profile();
    let t = fixtures::topology(true, true);
    let d = PrimarySelector::new(&p, &t).update(&p.routing, &t, Duration::ZERO);
    let a = compile_profile_with_primary(&p, &t, &ResolutionSnapshot::empty(), &d, 1).unwrap();
    let b = compile_profile_with_primary(&p, &t, &ResolutionSnapshot::empty(), &d, 2).unwrap();
    assert_eq!(a.content_hash(), b.content_hash());
    let mut backup = d.clone();
    backup.effective.v4 = FamilyExit::Lane("vinn".into());
    let c = compile_profile_with_primary(&p, &t, &ResolutionSnapshot::empty(), &backup, 3).unwrap();
    assert_ne!(a.content_hash(), c.content_hash());
    assert_eq!(
        a.lanes
            .iter()
            .map(|l| (&l.id, l.mark, l.table))
            .collect::<Vec<_>>(),
        c.lanes
            .iter()
            .map(|l| (&l.id, l.mark, l.table))
            .collect::<Vec<_>>()
    );
}

#[test]
fn local_flow_proof_includes_os_socket_sources_outside_profile_lanes() {
    let plan = compile(&fixtures::profile(), &fixtures::topology(true, true), 1);
    assert!(plan
        .local_addresses
        .contains(&"172.16.0.2".parse().unwrap()));
    assert!(plan
        .local_addresses
        .contains(&"2001:db8:3::2".parse().unwrap()));
    let mut missing_proof = plan.clone();
    missing_proof.local_addresses.clear();
    assert_ne!(plan.content_hash(), missing_proof.content_hash());
}

#[test]
fn unavailable_family_produces_terminal_owned_route_and_rule_commands() {
    use routelane::backend::build_terminal_policy_commands;
    let p = fixtures::profile();
    let plan = compile(
        &p,
        &fixtures::topology_case(true, true, false, false, "usb0", 3),
        1,
    );
    let commands = build_terminal_policy_commands(&plan);
    assert_eq!(commands.len(), 4);
    assert_eq!(commands[0].family, Some(IpFamily::V6));
    assert_eq!(
        commands[0].args,
        vec![
            "route",
            "replace",
            "unreachable",
            "default",
            "table",
            "20000"
        ]
    );
    assert_eq!(
        commands[1].args,
        vec![
            "route",
            "replace",
            "unreachable",
            "default",
            "table",
            "20001"
        ]
    );
    assert_eq!(
        commands[2].args,
        vec![
            "rule",
            "add",
            "fwmark",
            "0x524c0001",
            "lookup",
            "20000",
            "priority",
            "10000"
        ]
    );
    assert_eq!(
        commands[3].args,
        vec![
            "rule",
            "add",
            "fwmark",
            "0x524c0002",
            "lookup",
            "20001",
            "priority",
            "10001"
        ]
    );
}

#[test]
fn lane_direct_failure_uses_effective_primary_instead_of_os_bypass() {
    let mut p = fixtures::profile();
    p.lanes[1].failure_policy = LaneFailurePolicy::Direct;
    add_rule(&mut p, "vinn-site", 10, RuleAction::Route("vinn".into()));
    let plan = compile(&p, &fixtures::topology(true, false), 1);
    assert_eq!(
        action(&plan, "vinn-site", IpFamily::V4),
        Some(mark(&plan, "ethernet"))
    );
    assert_eq!(
        action(&plan, "vinn-site", IpFamily::V6),
        Some(mark(&plan, "ethernet"))
    );
}
