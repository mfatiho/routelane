use super::{KernelPlan, LaneAvailability};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Program {
    Ip,
    Nft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpFamily {
    V4,
    V6,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: Program,
    pub family: Option<IpFamily>,
    pub args: Vec<String>,
    pub stdin: Option<Vec<u8>>,
}

impl CommandSpec {
    pub fn ip4<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::ip(IpFamily::V4, args)
    }

    pub fn ip6<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::ip(IpFamily::V6, args)
    }

    fn ip<I, S>(family: IpFamily, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            program: Program::Ip,
            family: Some(family),
            args: args.into_iter().map(Into::into).collect(),
            stdin: None,
        }
    }
}

pub fn build_policy_commands(plan: &KernelPlan) -> Vec<CommandSpec> {
    let available_lanes = plan
        .lanes
        .iter()
        .filter(|lane| lane.availability == LaneAvailability::Available)
        .collect::<Vec<_>>();
    let mut commands = Vec::new();

    for lane in &available_lanes {
        if let Some(gateway) = lane.gateway_v4 {
            commands.push(CommandSpec::ip4([
                "route".to_owned(),
                "replace".to_owned(),
                "default".to_owned(),
                "via".to_owned(),
                gateway.to_string(),
                "dev".to_owned(),
                lane.interface.clone(),
                "table".to_owned(),
                lane.table.to_string(),
            ]));
        }
        if let Some(gateway) = lane.gateway_v6 {
            commands.push(CommandSpec::ip6([
                "route".to_owned(),
                "replace".to_owned(),
                "default".to_owned(),
                "via".to_owned(),
                gateway.to_string(),
                "dev".to_owned(),
                lane.interface.clone(),
                "table".to_owned(),
                lane.table.to_string(),
            ]));
        }
    }

    for lane in available_lanes {
        let args = || {
            [
                "rule".to_owned(),
                "add".to_owned(),
                "fwmark".to_owned(),
                format!("{:#010x}", lane.mark),
                "lookup".to_owned(),
                lane.table.to_string(),
                "priority".to_owned(),
                lane.rule_priority.to_string(),
            ]
        };
        if lane.gateway_v4.is_some() {
            commands.push(CommandSpec::ip4(args()));
        }
        if lane.gateway_v6.is_some() {
            commands.push(CommandSpec::ip6(args()));
        }
    }

    commands
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use crate::domain::{LaneFailurePolicy, LaneId, ProfileId};

    use super::{build_policy_commands, CommandSpec};
    use crate::backend::{KernelPlan, LaneAvailability, LanePlan};

    fn lane(
        id: &str,
        interface: &str,
        gateway_v4: Option<Ipv4Addr>,
        gateway_v6: Option<Ipv6Addr>,
        availability: LaneAvailability,
        table: u32,
        mark: u32,
        rule_priority: u32,
    ) -> LanePlan {
        LanePlan {
            id: LaneId::from(id),
            interface: interface.to_owned(),
            gateway_v4,
            gateway_v6,
            availability,
            table,
            mark,
            rule_priority,
            failure_policy: LaneFailurePolicy::Inactive,
        }
    }

    fn plan(lanes: Vec<LanePlan>) -> KernelPlan {
        KernelPlan {
            generation: 1,
            profile_id: ProfileId::from("default"),
            lanes,
            rules: vec![],
        }
    }

    #[test]
    fn builds_all_routes_before_policy_rules_in_lane_order() {
        let commands = build_policy_commands(&plan(vec![
            lane(
                "work",
                "wg0",
                Some("10.0.0.1".parse().unwrap()),
                Some("2001:db8::1".parse().unwrap()),
                LaneAvailability::Available,
                20_000,
                0x524c0001,
                10_000,
            ),
            lane(
                "privacy",
                "wg1",
                Some("10.1.0.1".parse().unwrap()),
                None,
                LaneAvailability::Available,
                20_001,
                0x524c0002,
                10_001,
            ),
        ]));

        assert_eq!(
            commands,
            vec![
                CommandSpec::ip4([
                    "route", "replace", "default", "via", "10.0.0.1", "dev", "wg0", "table",
                    "20000",
                ]),
                CommandSpec::ip6([
                    "route",
                    "replace",
                    "default",
                    "via",
                    "2001:db8::1",
                    "dev",
                    "wg0",
                    "table",
                    "20000",
                ]),
                CommandSpec::ip4([
                    "route", "replace", "default", "via", "10.1.0.1", "dev", "wg1", "table",
                    "20001",
                ]),
                CommandSpec::ip4([
                    "rule",
                    "add",
                    "fwmark",
                    "0x524c0001",
                    "lookup",
                    "20000",
                    "priority",
                    "10000",
                ]),
                CommandSpec::ip6([
                    "rule",
                    "add",
                    "fwmark",
                    "0x524c0001",
                    "lookup",
                    "20000",
                    "priority",
                    "10000",
                ]),
                CommandSpec::ip4([
                    "rule",
                    "add",
                    "fwmark",
                    "0x524c0002",
                    "lookup",
                    "20001",
                    "priority",
                    "10001",
                ]),
            ]
        );
    }

    #[test]
    fn skips_unavailable_lanes_and_missing_gateway_families() {
        let commands = build_policy_commands(&plan(vec![
            lane(
                "offline",
                "wg0",
                Some("10.0.0.1".parse().unwrap()),
                Some("2001:db8::1".parse().unwrap()),
                LaneAvailability::Unavailable,
                20_000,
                0x524c0001,
                10_000,
            ),
            lane(
                "v6-only",
                "wg1",
                None,
                Some("2001:db8:1::1".parse().unwrap()),
                LaneAvailability::Available,
                20_001,
                0x524c0002,
                10_001,
            ),
        ]));

        assert_eq!(
            commands,
            vec![
                CommandSpec::ip6([
                    "route",
                    "replace",
                    "default",
                    "via",
                    "2001:db8:1::1",
                    "dev",
                    "wg1",
                    "table",
                    "20001",
                ]),
                CommandSpec::ip6([
                    "rule",
                    "add",
                    "fwmark",
                    "0x524c0002",
                    "lookup",
                    "20001",
                    "priority",
                    "10001",
                ]),
            ]
        );
    }
}
