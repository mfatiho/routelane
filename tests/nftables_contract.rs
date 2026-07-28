use std::net::{Ipv4Addr, Ipv6Addr};

use ipnet::IpNet;
use routelane::backend::{
    render_ruleset, CompiledAction, CompiledRule, CompiledTarget, KernelPlan, LaneAvailability,
    LanePlan, NftRenderError, TableMode,
};
use routelane::domain::{LaneFailurePolicy, LaneId, ProfileId, RuleId};

fn fixture_kernel_plan_with_all_actions() -> KernelPlan {
    KernelPlan {
        generation: 1,
        profile_id: ProfileId::from("default"),
        lanes: vec![LanePlan {
            id: LaneId::from("privacy"),
            interface: "wg-privacy".into(),
            gateway_v4: Some(Ipv4Addr::new(10, 0, 0, 1)),
            gateway_v6: Some("2001:db8::1".parse::<Ipv6Addr>().unwrap()),
            availability: LaneAvailability::Available,
            table: 20_001,
            mark: 0x524c0002,
            rule_priority: 10_001,
            failure_policy: LaneFailurePolicy::Inactive,
        }],
        rules: vec![
            CompiledRule {
                id: RuleId::from("route"),
                priority: 10,
                target: CompiledTarget::Network("203.0.113.0/24".parse::<IpNet>().unwrap()),
                action: CompiledAction::Mark(0x524c0002),
            },
            CompiledRule {
                id: RuleId::from("direct"),
                priority: 20,
                target: CompiledTarget::Network("198.51.100.7/32".parse::<IpNet>().unwrap()),
                action: CompiledAction::Direct,
            },
            CompiledRule {
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
fn rejects_interface_value_that_cannot_be_a_kernel_interface() {
    let mut plan = fixture_kernel_plan_with_all_actions();
    plan.lanes[0].interface = "wg0\nflush ruleset".into();

    assert_eq!(
        render_ruleset(&plan, TableMode::Create),
        Err(NftRenderError::InvalidInterfaceName)
    );
}
