//! Selective transaction guard: while a transition is in flight, only the
//! managed scope (active ∪ pending explicit rules and RouteLane-owned flows)
//! is held back; unmatched traffic keeps flowing.

#[path = "support/nft_eval.rs"]
mod nft_eval;
#[path = "support/primary.rs"]
mod primary_fixtures;

use nft_eval::{execute, Packet};
use routelane::backend::{
    compile_profile, render_activated_nft_document, render_nft_document, KernelPlan,
    ResolutionSnapshot,
};
use routelane::domain::{
    ApplicationSelector, LaneId, Rule, RuleAction, RuleId, RuleSource, RuleTarget, UnmatchedTraffic,
};
use serde_json::Value;

const ETHERNET: u32 = 0x524c0001;

fn ip_rule(id: &str, address: &str, lane: &str) -> Rule {
    Rule {
        id: RuleId::from(id),
        enabled: true,
        priority: 10,
        source: RuleSource::Any,
        target: RuleTarget::Ip(address.parse().unwrap()),
        action: RuleAction::Route(LaneId::from(lane)),
    }
}

fn browser_rule() -> Rule {
    Rule {
        id: RuleId::from("browser"),
        enabled: true,
        priority: 5,
        source: RuleSource::Application(ApplicationSelector {
            desktop_id: None,
            executable_path: "/usr/bin/firefox".into(),
            include_children: true,
            launch: None,
        }),
        target: RuleTarget::Any,
        action: RuleAction::Route(LaneId::from("vinn")),
    }
}

fn plan(mode: UnmatchedTraffic, rules: Vec<Rule>, generation: u64) -> KernelPlan {
    let mut profile = primary_fixtures::profile();
    profile.routing.unmatched_traffic = mode;
    profile.rules = rules;
    compile_profile(
        &profile,
        &primary_fixtures::topology(true, true),
        &ResolutionSnapshot::empty(),
        generation,
    )
    .unwrap()
}

fn packet(destination: &'static str) -> Packet {
    Packet {
        source: "10.0.0.2",
        destination,
        ..Default::default()
    }
}

/// The guard chain's verdict for a packet (base chain output, priority -149).
fn guard(document: &Value, packet: &Packet) -> &'static str {
    execute(document, "guard", &mut packet.clone())
}

const UNMANAGED: [&str; 3] = ["10.65.201.60", "198.51.100.20", "172.20.5.5"];

#[test]
fn selective_guard_only_covers_active_pending_union() {
    let active = plan(
        UnmatchedTraffic::PreserveSystem,
        vec![ip_rule("old-site", "203.0.113.7", "vinn")],
        1,
    );
    let pending = plan(
        UnmatchedTraffic::PreserveSystem,
        vec![ip_rule("new-site", "198.51.100.9", "ethernet")],
        2,
    );
    let document = render_nft_document(Some(&active), Some(&pending), true);
    for managed in ["203.0.113.7", "198.51.100.9"] {
        assert_eq!(guard(&document, &packet(managed)), "reject", "{managed}");
    }
    for unmanaged in UNMANAGED {
        for (mark, ct_mark) in [(0, 0), (42, 0), (0, 42), (42, 43)] {
            let verdict = guard(
                &document,
                &Packet {
                    mark,
                    ct_mark,
                    ..packet(unmanaged)
                },
            );
            assert!(
                !matches!(verdict, "reject" | "drop"),
                "{unmanaged} {mark}/{ct_mark}: {verdict}"
            );
        }
    }
    // An owned flow cannot leak through the transition either.
    for (mark, ct_mark) in [(ETHERNET, 0), (0, ETHERNET)] {
        let owned = Packet {
            mark,
            ct_mark,
            ..packet("198.51.100.20")
        };
        assert_eq!(guard(&document, &owned), "reject", "{mark}/{ct_mark}");
    }
}

#[test]
fn empty_selective_guard_does_not_reject_unmatched() {
    let pending = plan(UnmatchedTraffic::PreserveSystem, vec![], 1);
    let document = render_nft_document(None, Some(&pending), true);
    for unmanaged in UNMANAGED {
        let verdict = guard(&document, &packet(unmanaged));
        assert!(
            !matches!(verdict, "reject" | "drop"),
            "{unmanaged}: {verdict}"
        );
    }
}

#[test]
fn cgroup_guard_uses_the_same_match_scope() {
    let pending = plan(UnmatchedTraffic::PreserveSystem, vec![browser_rule()], 1);
    let document = render_nft_document(None, Some(&pending), true);
    let selected = Packet {
        cgroup: "routelane.apps/browser",
        ..packet("198.51.100.20")
    };
    assert_eq!(guard(&document, &selected), "reject");
    let unselected = packet("198.51.100.20");
    assert!(!matches!(guard(&document, &unselected), "reject" | "drop"));
}

#[test]
fn activated_guard_still_holds_back_the_replaced_scope() {
    // Once the next generation's verdicts are live the guard stays until the
    // transition ends; a rollback may still reinstate the replaced scope, so
    // that scope must not leak through the OS default in the meantime.
    let replaced = plan(
        UnmatchedTraffic::PreserveSystem,
        vec![ip_rule("old-site", "203.0.113.7", "vinn")],
        1,
    );
    let next = plan(
        UnmatchedTraffic::PreserveSystem,
        vec![ip_rule("new-site", "198.51.100.9", "ethernet")],
        2,
    );
    let document = render_activated_nft_document(&next, Some(&replaced));
    for managed in ["203.0.113.7", "198.51.100.9"] {
        assert_eq!(guard(&document, &packet(managed)), "reject", "{managed}");
    }
    for unmanaged in UNMANAGED {
        let verdict = guard(&document, &packet(unmanaged));
        assert!(
            !matches!(verdict, "reject" | "drop"),
            "{unmanaged}: {verdict}"
        );
    }
    // Without a replaced generation it is the plain guarded document.
    assert_eq!(
        render_activated_nft_document(&next, None),
        render_nft_document(Some(&next), None, true)
    );
}

#[test]
fn use_primary_transition_keeps_the_broad_guard() {
    for (active_mode, pending_mode) in [
        (
            UnmatchedTraffic::UsePrimary,
            UnmatchedTraffic::PreserveSystem,
        ),
        (
            UnmatchedTraffic::PreserveSystem,
            UnmatchedTraffic::UsePrimary,
        ),
    ] {
        let active = plan(active_mode, vec![], 1);
        let pending = plan(pending_mode, vec![], 2);
        let document = render_nft_document(Some(&active), Some(&pending), true);
        assert_eq!(guard(&document, &packet("198.51.100.20")), "reject");
    }
}
