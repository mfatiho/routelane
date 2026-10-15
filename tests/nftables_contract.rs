use std::net::{Ipv4Addr, Ipv6Addr};

use ipnet::IpNet;
use routelane::backend::{
    render_ruleset, CompiledAction, CompiledRule, CompiledTarget, KernelPlan, LaneAvailability,
    LanePlan, NftRenderError, RouteMode, TableMode,
};
use routelane::domain::{LaneFailurePolicy, LaneId, ProfileId, RuleId};

#[path = "support/primary.rs"]
mod primary_fixtures;

mod affinity {
    use super::*;
    use routelane::backend::{compile_profile, render_nft_document, ResolutionSnapshot};
    use serde_json::{json, Value};
    use std::net::IpAddr;

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

    fn rules(document: &Value, chain: &str) -> Vec<Value> {
        document["nftables"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|o| o.get("rule"))
            .filter(|r| r["chain"] == chain)
            .map(|r| r["expr"].clone())
            .collect()
    }

    #[derive(Clone)]
    struct Packet {
        family: &'static str,
        source: &'static str,
        destination: &'static str,
        mark: u32,
        ct_mark: u32,
        state: &'static str,
        direction: &'static str,
        iif: u32,
        iifname: &'static str,
        oif: u32,
        oifname: &'static str,
        l4proto: &'static str,
        sport: u16,
        dport: u16,
        hoplimit: u8,
        icmp_type: &'static str,
        translated: bool,
    }

    impl Default for Packet {
        fn default() -> Self {
            Self {
                family: "ipv4",
                source: "192.168.0.2",
                destination: "203.0.113.10",
                mark: 0,
                ct_mark: 0,
                state: "new",
                direction: "original",
                iif: 0,
                iifname: "",
                oif: 2,
                oifname: "eth0",
                translated: false,
                l4proto: "tcp",
                sport: 0,
                dport: 0,
                hoplimit: 64,
                icmp_type: "echo-request",
            }
        }
    }

    fn value(expression: &Value, packet: &Packet) -> Value {
        if let Some(meta) = expression.get("meta") {
            return match meta["key"].as_str().unwrap() {
                "nfproto" => json!(packet.family),
                "mark" => json!(packet.mark),
                "iif" => json!(packet.iif),
                "iifname" => json!(packet.iifname),
                "oif" => json!(packet.oif),
                "oifname" => json!(packet.oifname),
                "l4proto" => json!(packet.l4proto),
                key => panic!("unsupported meta {key}"),
            };
        }
        if let Some(ct) = expression.get("ct") {
            return match ct["key"].as_str().unwrap() {
                "mark" => json!(packet.ct_mark),
                "direction" => json!(packet.direction),
                "state" => json!(packet.state),
                "ip saddr" | "ip6 saddr" => json!(packet.source),
                key => panic!("unsupported ct {key}"),
            };
        }
        if expression.get("fib").is_some() {
            return json!("unicast");
        }
        if expression.get("socket").is_some() {
            return json!("not-selected");
        }
        if let Some(payload) = expression.get("payload") {
            let protocol = payload["protocol"].as_str().unwrap();
            if (protocol == "ip" && packet.family != "ipv4")
                || (protocol == "ip6" && packet.family != "ipv6")
                || (protocol == "udp" && packet.l4proto != "udp")
                || (protocol == "icmpv6" && packet.l4proto != "ipv6-icmp")
            {
                return Value::Null;
            }
            return match payload["field"].as_str().unwrap() {
                "daddr" => json!(packet.destination),
                "sport" => json!(packet.sport),
                "dport" => json!(packet.dport),
                "hoplimit" => json!(packet.hoplimit),
                "type" => json!(packet.icmp_type),
                key => panic!("unsupported payload {key}"),
            };
        }
        expression.clone()
    }

    fn equals(left: &Value, right: &Value) -> bool {
        if let Some(range) = right.get("range").and_then(Value::as_array) {
            return left.as_u64().is_some_and(|n| {
                n >= range[0].as_u64().unwrap() && n <= range[1].as_u64().unwrap()
            });
        }
        if let (Some(n), Some(text)) = (left.as_u64(), right.as_str()) {
            let resolved = match text {
                "eth0" => Some(2),
                "usb0" => Some(3),
                _ => text.parse::<u64>().ok(),
            };
            if let Some(resolved) = resolved {
                return n == resolved;
            }
        }
        if let Some(set) = right.get("set") {
            return set.as_array().unwrap().iter().any(|r| equals(left, r));
        }
        if let Some(prefix) = right.get("prefix") {
            let net: IpNet = format!("{}/{}", prefix["addr"].as_str().unwrap(), prefix["len"])
                .parse()
                .unwrap();
            return left
                .as_str()
                .and_then(|s| s.parse::<IpAddr>().ok())
                .is_some_and(|ip| net.contains(&ip));
        }
        left == right
    }

    fn execute(document: &Value, chain: &str, packet: &mut Packet) -> &'static str {
        for rule in rules(document, chain) {
            let mut matched = true;
            for expression in rule.as_array().unwrap() {
                if let Some(m) = expression.get("match") {
                    let left = value(&m["left"], packet);
                    matched = match m["op"].as_str().unwrap() {
                        "==" | "in" => equals(&left, &m["right"]),
                        "!=" => !equals(&left, &m["right"]),
                        ">=" => left.as_u64().unwrap() >= m["right"].as_u64().unwrap(),
                        "<=" => left.as_u64().unwrap() <= m["right"].as_u64().unwrap(),
                        "<" => left.as_u64().unwrap() < m["right"].as_u64().unwrap(),
                        ">" => left.as_u64().unwrap() > m["right"].as_u64().unwrap(),
                        op => panic!("unsupported match {op}"),
                    };
                    if !matched {
                        break;
                    }
                } else if let Some(m) = expression.get("mangle") {
                    let n = value(&m["value"], packet).as_u64().unwrap() as u32;
                    if m["key"].get("meta").is_some() {
                        packet.mark = n;
                    } else {
                        packet.ct_mark = n;
                    }
                } else if let Some(goto) = expression.get("goto") {
                    return execute(document, goto["target"].as_str().unwrap(), packet);
                } else if let Some(jump) = expression.get("jump") {
                    let result = execute(document, jump["target"].as_str().unwrap(), packet);
                    if result != "return" {
                        return result;
                    }
                } else if expression.get("reject").is_some() {
                    return "reject";
                } else if expression.get("drop").is_some() {
                    return "drop";
                } else if expression.get("return").is_some() {
                    return "return";
                } else if expression.get("masquerade").is_some() {
                    packet.translated = true;
                    return "return";
                } else {
                    assert!(
                        expression.get("counter").is_some(),
                        "unsupported {expression}"
                    );
                }
            }
            let _ = matched;
        }
        "return"
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
}

fn fixture_kernel_plan_with_all_actions() -> KernelPlan {
    KernelPlan {
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
