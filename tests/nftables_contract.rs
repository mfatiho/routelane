use std::net::{Ipv4Addr, Ipv6Addr};

use ipnet::IpNet;
use routelane::backend::{
    render_ruleset, CompiledAction, CompiledRule, CompiledTarget, KernelPlan, LaneAvailability,
    LanePlan, NftRenderError, RouteMode, TableMode,
};
use routelane::domain::{LaneFailurePolicy, LaneId, ProfileId, RuleId};

#[path = "support/nft_eval.rs"]
mod nft_eval;
#[path = "support/primary.rs"]
mod primary_fixtures;

mod affinity {
    use super::nft_eval::{execute, rules, Packet};
    use super::*;
    use routelane::backend::{compile_profile, render_nft_document, ResolutionSnapshot};
    use serde_json::json;

    fn plan() -> KernelPlan {
        let mut p = compile_profile(
            &primary_fixtures::profile(),
            &primary_fixtures::topology(true, true),
            &ResolutionSnapshot::empty(),
            1,
        )
        .unwrap();
        p.local_networks = vec![
            "10.0.0.0/24".parse().unwrap(),
            "192.168.0.0/24".parse().unwrap(),
        ];
        p
    }

    #[test]
    fn text_and_json_render_same_primary_policy() {
        let mut p = plan();
        p.rules = vec![
            CompiledRule {
                family: routelane::backend::IpFamily::V4,
                id: "explicit".into(),
                priority: 10,
                target: CompiledTarget::Network("203.0.113.99/32".parse().unwrap()),
                action: CompiledAction::Mark(0x524c0002),
            },
            CompiledRule {
                family: routelane::backend::IpFamily::V6,
                id: "explicit-block".into(),
                priority: 20,
                target: CompiledTarget::Network("2001:db8:dead::/48".parse().unwrap()),
                action: CompiledAction::Block,
            },
        ];
        let document = render_nft_document(Some(&p), None, false);
        let text = render_ruleset(&p, TableMode::Create).unwrap();
        for (name, kind, hook, priority) in [
            ("output", "route", "output", -150),
            ("prerouting", "filter", "prerouting", -150),
            ("source_nat", "nat", "postrouting", 100),
        ] {
            assert!(document["nftables"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["chain"]["name"] == name
                    && o["chain"]["type"] == kind
                    && o["chain"]["hook"] == hook
                    && o["chain"]["prio"] == priority));
            assert!(text.contains(&format!("type {kind} hook {hook} priority {priority};")));
        }
        assert!(!text.contains("flush ruleset"));
        assert!(!text.contains("hook forward"));
        let mut packet = Packet::default();
        assert_eq!(execute(&document, "output", &mut packet), "return");
        assert_eq!((packet.mark, packet.ct_mark), (0x524c0001, 0x524c0001));
        assert!(text.contains("ct mark set meta mark"));
        assert!(rules(&document, "output")
            .iter()
            .any(|r| r.as_array().unwrap().contains(&json!({"counter":null}))));
        if std::env::var_os("ROUTELANE_EXPORT_NFT_FIXTURE").is_some() {
            println!(
                "NFT_EXPORT={}",
                json!({"document":document,"text":text,
                "snapshot_text":render_ruleset(&fixture_kernel_plan_with_all_actions(),TableMode::Create).unwrap()})
            );
        }
    }

    #[test]
    fn existing_backup_flow_keeps_mark_after_failback() {
        let document = render_nft_document(Some(&plan()), None, false);
        let mut old = Packet {
            ct_mark: 0x524c0002,
            state: "established",
            ..Default::default()
        };
        assert_eq!(execute(&document, "output", &mut old), "return");
        assert_eq!((old.mark, old.ct_mark), (0x524c0002, 0x524c0002));
        let mut new = Packet::default();
        execute(&document, "output", &mut new);
        assert_eq!(new.mark, 0x524c0001);
        let mut down = plan();
        down.lanes[1].route_v4 = None;
        down.lanes[1].availability = LaneAvailability::Unavailable;
        assert_eq!(
            execute(
                &render_nft_document(Some(&down), None, false),
                "output",
                &mut old
            ),
            "reject"
        );
    }

    #[test]
    fn explicit_block_precedes_flow_restore() {
        let mut p = plan();
        p.rules = vec![CompiledRule {
            family: routelane::backend::IpFamily::V4,
            id: "block".into(),
            priority: 1,
            target: CompiledTarget::Any,
            action: CompiledAction::Block,
        }];
        let mut packet = Packet {
            ct_mark: 0x524c0002,
            state: "established",
            ..Default::default()
        };
        assert_eq!(
            execute(
                &render_nft_document(Some(&p), None, false),
                "output",
                &mut packet
            ),
            "reject"
        );
        p.rules.insert(
            0,
            CompiledRule {
                family: routelane::backend::IpFamily::V4,
                id: "first-route".into(),
                priority: 0,
                target: CompiledTarget::Any,
                action: CompiledAction::Mark(0x524c0001),
            },
        );
        assert_eq!(
            execute(
                &render_nft_document(Some(&p), None, false),
                "output",
                &mut packet
            ),
            "return"
        );
        assert_eq!(
            packet.mark, 0x524c0002,
            "later Block must not move before the first Route verdict"
        );
    }

    #[test]
    fn local_and_control_exemptions_are_narrow() {
        let p = plan();
        let document = render_nft_document(Some(&p), None, false);
        for destination in ["10.0.0.50", "192.168.0.1"] {
            let mut packet = Packet {
                destination,
                ..Default::default()
            };
            execute(&document, "output", &mut packet);
            assert_eq!(packet.mark, 0, "only observed connected LAN is bypassed");
        }
        let mut private_remote = Packet {
            destination: "10.99.0.1",
            ..Default::default()
        };
        execute(&document, "output", &mut private_remote);
        assert_eq!(
            private_remote.mark, 0x524c0001,
            "do not exempt all RFC1918 networks"
        );
        let mut p = p;
        p.rules.push(CompiledRule {
            family: routelane::backend::IpFamily::V4,
            id: "local-block".into(),
            priority: 0,
            target: CompiledTarget::Network("10.0.0.50/32".parse().unwrap()),
            action: CompiledAction::Block,
        });
        let mut lan = Packet {
            destination: "10.0.0.50",
            ..Default::default()
        };
        assert_eq!(
            execute(
                &render_nft_document(Some(&p), None, false),
                "output",
                &mut lan
            ),
            "reject"
        );
    }

    #[test]
    fn nat_requires_exact_owned_mark_and_interface() {
        let document = render_nft_document(Some(&plan()), None, false);
        let mut packet = Packet {
            mark: 0x524c0001,
            ct_mark: 0x524c0001,
            ..Default::default()
        };
        execute(&document, "source_nat", &mut packet);
        assert!(packet.translated);
        for mut wrong in [
            Packet {
                oif: 3,
                oifname: "usb0",
                ..packet.clone()
            },
            Packet {
                iif: 7,
                iifname: "docker0",
                ..packet.clone()
            },
            Packet {
                ct_mark: 42,
                ..packet.clone()
            },
            Packet {
                source: "198.51.100.20",
                ..packet.clone()
            },
        ] {
            wrong.translated = false;
            execute(&document, "source_nat", &mut wrong);
            assert!(
                !wrong.translated,
                "forwarded/foreign/wrong-egress traffic must never be NATed"
            );
        }
        let mut race = Packet {
            oif: 3,
            oifname: "usb0",
            ..packet
        };
        assert_eq!(
            execute(&document, "egress_guard", &mut race),
            "drop",
            "removed policy routes must not fall through to OS default"
        );
    }

    #[test]
    fn foreign_marks_are_not_overwritten() {
        let document = render_nft_document(Some(&plan()), None, false);
        for (mark, ct_mark) in [(42, 0), (0, 42)] {
            let mut packet = Packet {
                mark,
                ct_mark,
                ..Default::default()
            };
            assert_eq!(execute(&document, "output", &mut packet), "reject");
            assert_eq!((packet.mark, packet.ct_mark), (mark, ct_mark));
        }
        let mut forwarded = Packet {
            iif: 7,
            iifname: "docker0",
            direction: "reply",
            ct_mark: 0x524c0001,
            source: "198.51.100.20",
            ..Default::default()
        };
        execute(&document, "prerouting", &mut forwarded);
        assert_eq!(forwarded.mark, 0);
    }

    #[test]
    fn existing_unmarked_sessions_are_not_rebound_or_source_translated() {
        let document = render_nft_document(Some(&plan()), None, false);
        let mut existing = Packet {
            state: "established",
            ..Default::default()
        };
        execute(&document, "output", &mut existing);
        assert_eq!((existing.mark, existing.ct_mark), (0, 0));
        let mut inbound_reply = Packet {
            direction: "reply",
            ..Default::default()
        };
        execute(&document, "output", &mut inbound_reply);
        assert_eq!((inbound_reply.mark, inbound_reply.ct_mark), (0, 0));
    }

    #[test]
    fn dhcp_renewal_and_nd_survive_block_without_remote_udp_bypass() {
        let mut p = plan();
        p.rules = [
            routelane::backend::IpFamily::V4,
            routelane::backend::IpFamily::V6,
        ]
        .into_iter()
        .map(|family| CompiledRule {
            family,
            id: "block".into(),
            priority: 0,
            target: CompiledTarget::Any,
            action: CompiledAction::Block,
        })
        .collect();
        let document = render_nft_document(Some(&p), None, false);
        for destination in ["255.255.255.255", "192.168.0.1"] {
            let mut dhcp = Packet {
                l4proto: "udp",
                sport: 68,
                dport: 67,
                destination,
                ..Default::default()
            };
            assert_eq!(execute(&document, "output", &mut dhcp), "return");
            assert_eq!(dhcp.mark, 0);
        }
        let mut remote = Packet {
            l4proto: "udp",
            sport: 68,
            dport: 67,
            destination: "10.99.0.1",
            ..Default::default()
        };
        assert_eq!(execute(&document, "output", &mut remote), "reject");
        let mut nd = Packet {
            family: "ipv6",
            l4proto: "ipv6-icmp",
            hoplimit: 255,
            icmp_type: "nd-neighbor-advert",
            destination: "2001:db8:1::1",
            ..Default::default()
        };
        assert_eq!(execute(&document, "output", &mut nd), "return");
        nd.hoplimit = 64;
        assert_eq!(execute(&document, "output", &mut nd), "reject");
    }

    #[test]
    fn source_proof_is_numeric_sorted_for_exact_kernel_set_roundtrip() {
        let mut p = plan();
        p.local_addresses = vec!["10.0.0.10".parse().unwrap(), "10.0.0.2".parse().unwrap()];
        let document = render_nft_document(Some(&p), None, false);
        let nat = rules(&document, "source_nat");
        assert!(
            nat[0]
                .as_array()
                .unwrap()
                .iter()
                .any(|expr| expr["match"]["right"] == json!({"set":["10.0.0.2","10.0.0.10"]})),
            "kernel sorts IP sets numerically, not lexically"
        );
    }

    #[test]
    fn inherited_owned_marks_do_not_drop_neighbor_discovery_or_dhcp() {
        let document = render_nft_document(Some(&plan()), None, false);
        let mut nd = Packet {
            family: "ipv6",
            l4proto: "ipv6-icmp",
            hoplimit: 255,
            icmp_type: "nd-neighbor-solicit",
            source: "fe80::2",
            destination: "ff02::1:ff00:1",
            mark: 0x524c0001,
            ct_mark: 0,
            ..Default::default()
        };
        assert_eq!(execute(&document, "egress_guard", &mut nd), "return");
        let mut dhcp = Packet {
            l4proto: "udp",
            sport: 68,
            dport: 67,
            destination: "255.255.255.255",
            mark: 0x524c0001,
            ct_mark: 0,
            ..Default::default()
        };
        assert_eq!(execute(&document, "egress_guard", &mut dhcp), "return");
    }

    #[test]
    fn cidr_host_bits_are_masked_for_exact_kernel_prefix_roundtrip() {
        let mut p = plan();
        p.rules.push(CompiledRule {
            family: routelane::backend::IpFamily::V4,
            id: "cidr".into(),
            priority: 10,
            target: CompiledTarget::Network("203.0.113.99/24".parse().unwrap()),
            action: CompiledAction::Block,
        });
        let document = render_nft_document(Some(&p), None, false);
        assert!(rules(&document, "output")
            .iter()
            .any(|rule| rule
                .as_array()
                .unwrap()
                .iter()
                .any(|expr| expr["match"]["right"]
                    == json!({"prefix":{"addr":"203.0.113.0","len":24}}))));
    }

    #[test]
    fn interface_predicates_use_frozen_indices_not_runtime_name_lookup() {
        let document = render_nft_document(Some(&plan()), None, false);
        let nat = rules(&document, "source_nat");
        assert!(nat[0]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |expr| expr["match"]["left"] == json!({"meta":{"key":"oif"}})
                    && expr["match"]["right"] == 2
            ));
        assert!(nat[0]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |expr| expr["match"]["left"] == json!({"meta":{"key":"iif"}})
                    && expr["match"]["right"] == 0
            ));
        let mut reused_name = Packet {
            mark: 0x524c0001,
            ct_mark: 0x524c0001,
            oif: 9,
            oifname: "eth0",
            ..Default::default()
        };
        execute(&document, "source_nat", &mut reused_name);
        assert!(!reused_name.translated);
        assert_eq!(execute(&document, "egress_guard", &mut reused_name), "drop");
    }
}

fn fixture_kernel_plan_with_all_actions() -> KernelPlan {
    KernelPlan {
        unmatched_traffic: routelane::domain::UnmatchedTraffic::UsePrimary,
        primary: routelane::backend::PrimaryDecision {
            preferred: routelane::backend::FamilyPair {
                v4: Some("privacy".into()),
                v6: Some("privacy".into()),
            },
            effective: routelane::backend::FamilyPair {
                v4: routelane::backend::FamilyExit::Lane("privacy".into()),
                v6: routelane::backend::FamilyExit::Lane("privacy".into()),
            },
        },
        local_networks: vec![],
        local_addresses: vec!["10.0.0.2".parse().unwrap(), "2001:db8::2".parse().unwrap()],
        generation: 1,
        profile_id: ProfileId::from("default"),
        lanes: vec![LanePlan {
            identity: Some(routelane::backend::InterfaceIdentity {
                ifindex: 2,
                name: "wg-privacy".into(),
                kind: "wireguard".into(),
                hardware_address: None,
            }),
            source_v4: vec!["10.0.0.2".parse().unwrap()],
            source_v6: vec!["2001:db8::2".parse().unwrap()],
            id: LaneId::from("privacy"),
            interface: "wg-privacy".into(),
            route_v4: Some(RouteMode::Via(Ipv4Addr::new(10, 0, 0, 1))),
            route_v6: Some(RouteMode::Via("2001:db8::1".parse::<Ipv6Addr>().unwrap())),
            availability: LaneAvailability::Available,
            table: 20_001,
            mark: 0x524c0002,
            rule_priority: 10_001,
            failure_policy: LaneFailurePolicy::Inactive,
        }],
        rules: vec![
            CompiledRule {
                family: routelane::backend::IpFamily::V4,
                id: RuleId::from("route"),
                priority: 10,
                target: CompiledTarget::Network("203.0.113.0/24".parse::<IpNet>().unwrap()),
                action: CompiledAction::Mark(0x524c0002),
            },
            CompiledRule {
                family: routelane::backend::IpFamily::V4,
                id: RuleId::from("direct"),
                priority: 20,
                target: CompiledTarget::Network("198.51.100.7/32".parse::<IpNet>().unwrap()),
                action: CompiledAction::Direct,
            },
            CompiledRule {
                family: routelane::backend::IpFamily::V6,
                id: RuleId::from("block"),
                priority: 30,
                target: CompiledTarget::Network("2001:db8:dead::/48".parse::<IpNet>().unwrap()),
                action: CompiledAction::Block,
            },
        ],
    }
}

#[test]
fn renders_route_direct_and_enabled_block_in_priority_order() {
    let plan = fixture_kernel_plan_with_all_actions();

    let rendered = render_ruleset(&plan, TableMode::Create).unwrap();

    assert_eq!(rendered, include_str!("fixtures/nftables/three-lane.nft"));
}

#[test]
fn source_proof_rules_carry_no_redundant_family_dependency() {
    // `ct original ip saddr`/`ip6 saddr` load family-specific conntrack keys
    // that only match their own family. nft 1.1.6 (Ubuntu 26.04) hides an
    // explicit nfproto in front of them in some rules but not others, so a
    // dump of that form cannot prove the exact committed document.
    let rendered = render_ruleset(&fixture_kernel_plan_with_all_actions(), TableMode::Create)
        .expect("render source proof rules");
    let source_rules: Vec<&str> = rendered
        .lines()
        .filter(|line| line.contains("ct original ip"))
        .collect();
    assert_eq!(source_rules.len(), 6);
    for line in source_rules {
        assert!(!line.contains("meta nfproto"), "{line}");
    }
}

#[test]
fn replace_batch_deletes_and_recreates_only_routelane_table() {
    let rendered =
        render_ruleset(&fixture_kernel_plan_with_all_actions(), TableMode::Replace).unwrap();

    assert!(rendered.starts_with("delete table inet routelane\nadd table inet routelane\n"));
    assert!(!rendered.contains("flush ruleset"));
}

#[test]
fn block_counter_precedes_the_terminal_reject_verdict() {
    let rendered = render_ruleset(&fixture_kernel_plan_with_all_actions(), TableMode::Create)
        .expect("render block rule");

    assert!(
        rendered.contains("ip6 daddr 2001:db8:dead::/48 counter reject"),
        "a terminal reject before the counter prevents the block metric from increasing"
    );
}

#[test]
fn direct_rule_counts_before_terminal_primary_selection() {
    let rendered = render_ruleset(&fixture_kernel_plan_with_all_actions(), TableMode::Create)
        .expect("render direct rule");

    assert!(
        rendered.contains("ip daddr \"198.51.100.7\" counter goto select4_524c0002"),
        "Direct must count the first verdict and select the effective primary, never clear a foreign mark"
    );
}

#[test]
fn routed_rule_returns_after_marking_so_first_matching_rule_wins() {
    let mut plan = fixture_kernel_plan_with_all_actions();
    plan.rules = vec![
        CompiledRule {
            family: routelane::backend::IpFamily::V4,
            id: RuleId::from("specific-route"),
            priority: 10,
            target: CompiledTarget::Network("203.0.113.7/32".parse::<IpNet>().unwrap()),
            action: CompiledAction::Mark(0x524c0002),
        },
        CompiledRule {
            family: routelane::backend::IpFamily::V4,
            id: RuleId::from("catch-all-direct"),
            priority: 100,
            target: CompiledTarget::Any,
            action: CompiledAction::Direct,
        },
    ];

    let rendered = render_ruleset(&plan, TableMode::Create).expect("render overlapping rules");
    let specific = rendered
        .lines()
        .find(|line| line.contains("203.0.113.7"))
        .expect("specific route rule");
    let catch_all = rendered
        .lines()
        .find(|line| {
            *line ==
        "add rule inet routelane output meta nfproto \"ipv4\" counter goto select4_524c0002"
        })
        .expect("catch-all rule");

    assert_eq!(
        specific,
        "add rule inet routelane output ip daddr \"203.0.113.7\" counter goto select4_524c0002"
    );
    assert_eq!(
        catch_all,
        "add rule inet routelane output meta nfproto \"ipv4\" counter goto select4_524c0002"
    );
}

#[test]
fn renders_application_cgroup_before_destination_rules() {
    let mut plan = fixture_kernel_plan_with_all_actions();
    plan.rules.insert(
        0,
        CompiledRule {
            family: routelane::backend::IpFamily::V4,
            id: RuleId::from("browser"),
            priority: 1,
            target: CompiledTarget::Cgroup("routelane.apps/browser".to_owned()),
            action: CompiledAction::Mark(0x524c0002),
        },
    );

    let rendered = render_ruleset(&plan, TableMode::Create).unwrap();
    let application = rendered
        .lines()
        .find(|line| line.contains("socket cgroupv2"))
        .expect("application cgroup rule");
    let destination = rendered
        .lines()
        .find(|line| line.contains("203.0.113.0/24"))
        .expect("destination rule");

    assert_eq!(
        application,
        "add rule inet routelane output meta nfproto \"ipv4\" socket cgroupv2 level 2 \"routelane.apps/browser\" counter goto select4_524c0002"
    );
    assert!(rendered.find(application).unwrap() < rendered.find(destination).unwrap());
}

#[test]
fn rejects_interface_value_that_cannot_be_a_kernel_interface() {
    let mut plan = fixture_kernel_plan_with_all_actions();
    plan.lanes[0].interface = "wg0\nflush ruleset".into();

    assert_eq!(
        render_ruleset(&plan, TableMode::Create),
        Err(NftRenderError::InvalidInterfaceName)
    );
}
