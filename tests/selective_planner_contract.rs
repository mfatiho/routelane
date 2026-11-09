//! Selective routing: traffic that no enabled rule matches keeps the
//! operating system's own routing; explicit rules keep their order and scope.

#[path = "support/nft_eval.rs"]
mod nft_eval;
#[path = "support/primary.rs"]
mod primary_fixtures;

use nft_eval::{execute, Packet};
use routelane::backend::{
    compile_profile, policy_hash, render_nft_document, KernelPlan, ResolutionSnapshot,
    TopologySnapshot,
};
use routelane::domain::{
    ApplicationSelector, LaneId, Profile, Rule, RuleAction, RuleId, RuleSource, RuleTarget,
    UnmatchedTraffic,
};
use serde_json::Value;

const ETHERNET: u32 = 0x524c0001;
const VINN: u32 = 0x524c0002;

fn rule(id: &str, priority: u32, target: RuleTarget, action: RuleAction) -> Rule {
    Rule {
        id: RuleId::from(id),
        enabled: true,
        priority,
        source: RuleSource::Any,
        target,
        action,
    }
}

fn ip(address: &str) -> RuleTarget {
    RuleTarget::Ip(address.parse().unwrap())
}

fn cidr(network: &str) -> RuleTarget {
    RuleTarget::Cidr(network.parse().unwrap())
}

fn route(lane: &str) -> RuleAction {
    RuleAction::Route(LaneId::from(lane))
}

fn browser_rule(priority: u32, action: RuleAction) -> Rule {
    Rule {
        id: RuleId::from("browser"),
        enabled: true,
        priority,
        source: RuleSource::Application(ApplicationSelector {
            desktop_id: None,
            executable_path: "/usr/bin/firefox".into(),
            include_children: true,
            launch: None,
        }),
        target: RuleTarget::Any,
        action,
    }
}

fn profile(mode: UnmatchedTraffic, rules: Vec<Rule>) -> Profile {
    let mut profile = primary_fixtures::profile();
    profile.routing.unmatched_traffic = mode;
    profile.rules = rules;
    profile
}

fn plan_on(profile: &Profile, topology: &TopologySnapshot) -> KernelPlan {
    compile_profile(profile, topology, &ResolutionSnapshot::empty(), 1).unwrap()
}

fn plan(profile: &Profile) -> KernelPlan {
    plan_on(profile, &primary_fixtures::topology(true, true))
}

fn document(plan: &KernelPlan) -> Value {
    render_nft_document(Some(plan), None, false)
}

fn packet(destination: &'static str) -> Packet {
    if destination.contains(':') {
        Packet {
            family: "ipv6",
            source: "2001:db8:1::2",
            destination,
            ..Default::default()
        }
    } else {
        Packet {
            source: "10.0.0.2",
            destination,
            ..Default::default()
        }
    }
}

/// Local output, then the postrouting egress guard and source NAT, as the
/// kernel would see one new connection's first packet. Stops on a drop/reject.
fn walk(document: &Value, packet: &mut Packet) -> Vec<&'static str> {
    let mut verdicts = vec![];
    for chain in ["output", "egress_guard", "source_nat"] {
        let verdict = execute(document, chain, packet);
        verdicts.push(verdict);
        if matches!(verdict, "reject" | "drop") {
            break;
        }
    }
    verdicts
}

const UNMATCHED: [&str; 4] = [
    "10.65.201.60",      // lab bridge on another subnet
    "198.51.100.20",     // public
    "172.20.5.5",        // remote private network
    "2001:db8:ffff::10", // public IPv6
];

#[test]
fn selective_unmatched_dual_stack_preserves_packet() {
    let selective = profile(
        UnmatchedTraffic::PreserveSystem,
        vec![rule("site", 10, ip("203.0.113.7"), route("vinn"))],
    );
    let lanes_without_ipv6 = primary_fixtures::topology_case(true, true, false, false, "usb0", 3);
    for plan in [plan(&selective), plan_on(&selective, &lanes_without_ipv6)] {
        let document = document(&plan);
        for destination in UNMATCHED {
            for (mark, ct_mark) in [(0, 0), (42, 0), (0, 42), (42, 43)] {
                let mut packet = Packet {
                    mark,
                    ct_mark,
                    ..packet(destination)
                };
                let verdicts = walk(&document, &mut packet);
                assert!(
                    !verdicts.iter().any(|v| matches!(*v, "reject" | "drop")),
                    "{destination} {mark}/{ct_mark}: {verdicts:?}"
                );
                assert_eq!(
                    (packet.mark, packet.ct_mark, packet.translated),
                    (mark, ct_mark, false),
                    "{destination} {mark}/{ct_mark}"
                );
            }
        }
    }
}

#[test]
fn selective_foreign_mark_is_only_rejected_on_an_explicit_route() {
    let selective = profile(
        UnmatchedTraffic::PreserveSystem,
        vec![
            rule("routed", 10, ip("203.0.113.7"), route("vinn")),
            rule("direct", 20, ip("203.0.113.8"), RuleAction::Direct),
            rule("blocked", 30, ip("203.0.113.9"), RuleAction::Block),
        ],
    );
    let document = document(&plan(&selective));
    for destination in ["203.0.113.7", "203.0.113.8", "203.0.113.9"] {
        for (mark, ct_mark) in [(42, 0), (0, 42), (42, 43)] {
            let mut foreign = Packet {
                mark,
                ct_mark,
                ..packet(destination)
            };
            assert_eq!(
                execute(&document, "output", &mut foreign),
                "reject",
                "{destination} {mark}/{ct_mark}"
            );
            assert_eq!((foreign.mark, foreign.ct_mark), (mark, ct_mark));
        }
    }
    // A clean packet on the explicit route is still steered to its lane.
    let mut routed = packet("203.0.113.7");
    assert_eq!(execute(&document, "output", &mut routed), "return");
    assert_eq!((routed.mark, routed.ct_mark), (VINN, VINN));
    let mut direct = packet("203.0.113.8");
    execute(&document, "output", &mut direct);
    assert_eq!(direct.mark, ETHERNET, "Direct uses the effective primary");
    let mut blocked = packet("203.0.113.9");
    assert_eq!(execute(&document, "output", &mut blocked), "reject");
}

#[test]
fn explicit_rule_order_and_cgroup_scope_are_preserved() {
    let selective = profile(
        UnmatchedTraffic::PreserveSystem,
        vec![
            browser_rule(5, route("vinn")),
            rule("wide-route", 10, cidr("203.0.113.0/24"), route("vinn")),
            rule("later-block", 20, ip("203.0.113.7"), RuleAction::Block),
            rule("wide-block", 30, cidr("198.51.100.0/24"), RuleAction::Block),
            rule("later-route", 40, ip("198.51.100.9"), route("ethernet")),
        ],
    );
    let document = document(&plan(&selective));

    let mut earlier_route = packet("203.0.113.7");
    assert_eq!(execute(&document, "output", &mut earlier_route), "return");
    assert_eq!(earlier_route.mark, VINN, "the earlier Route wins");

    let mut earlier_block = packet("198.51.100.9");
    assert_eq!(execute(&document, "output", &mut earlier_block), "reject");

    let mut selected = Packet {
        cgroup: "routelane.apps/browser",
        ..packet("192.0.2.80")
    };
    execute(&document, "output", &mut selected);
    assert_eq!(selected.mark, VINN, "the selected application is routed");

    let mut unselected = packet("192.0.2.80");
    let verdicts = walk(&document, &mut unselected);
    assert!(!verdicts.contains(&"reject"), "{verdicts:?}");
    assert_eq!((unselected.mark, unselected.ct_mark), (0, 0));

    // Unmarked established and inbound-reply traffic stays unmanaged even
    // when its destination matches an explicit rule.
    for (state, direction) in [("established", "original"), ("new", "reply")] {
        let mut existing = Packet {
            state,
            direction,
            ..packet("203.0.113.7")
        };
        assert_eq!(execute(&document, "output", &mut existing), "return");
        assert_eq!(
            (existing.mark, existing.ct_mark),
            (0, 0),
            "{state}/{direction}"
        );
    }
}

#[test]
fn mode_changes_plan_and_policy_hashes() {
    let rules = vec![rule("site", 10, ip("203.0.113.7"), route("vinn"))];
    let selective = profile(UnmatchedTraffic::PreserveSystem, rules.clone());
    let use_primary = profile(UnmatchedTraffic::UsePrimary, rules);
    let selective_plan = plan(&selective);
    let use_primary_plan = plan(&use_primary);
    assert_eq!(
        selective_plan.unmatched_traffic,
        UnmatchedTraffic::PreserveSystem
    );
    assert_ne!(
        selective_plan.content_hash(),
        use_primary_plan.content_hash()
    );
    assert_ne!(
        policy_hash(&selective, &ResolutionSnapshot::empty()),
        policy_hash(&use_primary, &ResolutionSnapshot::empty())
    );
    // The hash still ignores the generation.
    let mut later = selective_plan.clone();
    later.generation += 1;
    assert_eq!(later.content_hash(), selective_plan.content_hash());
}

#[test]
fn use_primary_keeps_sending_unmatched_traffic_to_the_primary() {
    let use_primary = profile(
        UnmatchedTraffic::UsePrimary,
        vec![rule("site", 10, ip("203.0.113.7"), route("vinn"))],
    );
    let document = document(&plan(&use_primary));
    let mut unmatched = packet("198.51.100.20");
    execute(&document, "output", &mut unmatched);
    assert_eq!((unmatched.mark, unmatched.ct_mark), (ETHERNET, ETHERNET));
    let mut foreign = Packet {
        mark: 42,
        ..packet("198.51.100.20")
    };
    assert_eq!(execute(&document, "output", &mut foreign), "reject");
}
