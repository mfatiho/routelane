use std::net::IpAddr;

use super::{KernelPlan, LaneAvailability, LanePlan};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Program {
    Ip,
    Nft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

    pub(crate) fn ip<I, S>(family: IpFamily, args: I) -> Self
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

/// Metric of the lowest-preference unreachable default each lane table owns.
///
/// It is deliberately not `u32::MAX`: iproute2 treats an IPv6 unreachable
/// default with that metric as a table's null entry and skips it on flush.
pub const TERMINAL_ROUTE_METRIC: u32 = 4_294_967_294;

const FAMILIES: [IpFamily; 2] = [IpFamily::V4, IpFamily::V6];

/// One owned default route of a lane table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedRoute {
    Unicast {
        gateway: Option<IpAddr>,
        interface: String,
    },
    /// Fails marked traffic closed once the kernel removes the unicast route
    /// (link or address loss) instead of falling through to later tables.
    Terminal,
}

/// A single typed routing mutation. Commands and crash-state models are both
/// derived from these steps so they cannot drift apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutingStep {
    ReplaceRoute {
        family: IpFamily,
        table: u32,
        route: OwnedRoute,
    },
    /// Removes exactly this route if it is still present.
    DeleteRoute {
        family: IpFamily,
        table: u32,
        route: OwnedRoute,
    },
    AddRule {
        family: IpFamily,
        priority: u32,
        table: u32,
        mark: u32,
    },
    /// Removes exactly this rule if it is still present.
    DeleteRule {
        family: IpFamily,
        priority: u32,
        table: u32,
        mark: u32,
    },
    /// Flushes a whole reserved table if it still has entries.
    FlushTable { family: IpFamily, table: u32 },
}

impl RoutingStep {
    pub fn command(&self) -> CommandSpec {
        let rule = |verb: &str, family: IpFamily, priority: u32, table: u32, mark: u32| {
            CommandSpec::ip(
                family,
                [
                    "rule".to_owned(),
                    verb.to_owned(),
                    "fwmark".to_owned(),
                    format!("{mark:#010x}"),
                    "lookup".to_owned(),
                    table.to_string(),
                    "priority".to_owned(),
                    priority.to_string(),
                ],
            )
        };
        match self {
            Self::ReplaceRoute {
                family,
                table,
                route,
            } => route_command("replace", *family, *table, route, false),
            Self::DeleteRoute {
                family,
                table,
                route,
            } => route_command("del", *family, *table, route, true),
            Self::AddRule {
                family,
                priority,
                table,
                mark,
            } => rule("add", *family, *priority, *table, *mark),
            Self::DeleteRule {
                family,
                priority,
                table,
                mark,
            } => rule("del", *family, *priority, *table, *mark),
            Self::FlushTable { family, table } => CommandSpec::ip(
                *family,
                [
                    "route".to_owned(),
                    "flush".to_owned(),
                    "table".to_owned(),
                    table.to_string(),
                ],
            ),
        }
    }
}

fn route_command(
    verb: &str,
    family: IpFamily,
    table: u32,
    route: &OwnedRoute,
    exact_metric: bool,
) -> CommandSpec {
    let mut args = vec!["route".to_owned(), verb.to_owned()];
    match route {
        OwnedRoute::Unicast { gateway, interface } => {
            args.push("default".to_owned());
            if let Some(gateway) = gateway {
                args.extend(["via".to_owned(), gateway.to_string()]);
            }
            args.extend([
                "dev".to_owned(),
                interface.clone(),
                "table".to_owned(),
                table.to_string(),
            ]);
            if exact_metric {
                let metric = match family {
                    IpFamily::V4 => 0,
                    IpFamily::V6 => 1024,
                };
                args.extend(["metric".to_owned(), metric.to_string()]);
            }
        }
        OwnedRoute::Terminal => args.extend([
            "unreachable".to_owned(),
            "default".to_owned(),
            "table".to_owned(),
            table.to_string(),
            "metric".to_owned(),
            TERMINAL_ROUTE_METRIC.to_string(),
        ]),
    }
    CommandSpec::ip(family, args)
}

/// The unicast default a lane owns for a family, if the lane is usable for it.
pub fn lane_unicast_route(lane: &LanePlan, family: IpFamily) -> Option<OwnedRoute> {
    if lane.availability != LaneAvailability::Available {
        return None;
    }
    let gateway = match family {
        IpFamily::V4 => lane.route_v4?.gateway().copied().map(IpAddr::V4),
        IpFamily::V6 => lane.route_v6?.gateway().copied().map(IpAddr::V6),
    };
    Some(OwnedRoute::Unicast {
        gateway,
        interface: lane.interface.clone(),
    })
}

fn rule_step(lane: &LanePlan, family: IpFamily) -> RoutingStep {
    RoutingStep::AddRule {
        family,
        priority: lane.rule_priority,
        table: lane.table,
        mark: lane.mark,
    }
}

fn lane_for_table(plan: &KernelPlan, table: u32) -> Option<&LanePlan> {
    plan.lanes.iter().find(|lane| lane.table == table)
}

/// Steps that create a plan's complete routing state from an empty kernel.
pub fn install_steps(plan: &KernelPlan) -> Vec<RoutingStep> {
    transition_steps(None, plan)
}

/// Steps that move the routing state from `previous` to `next` before nftables
/// activation. Every lane×family owns a terminal route and a policy rule.
pub fn transition_steps(previous: Option<&KernelPlan>, next: &KernelPlan) -> Vec<RoutingStep> {
    let mut steps = Vec::new();
    for lane in &next.lanes {
        let previous_lane = previous.and_then(|plan| lane_for_table(plan, lane.table));
        for family in FAMILIES {
            let unicast = lane_unicast_route(lane, family);
            let previous_unicast =
                previous_lane.and_then(|previous| lane_unicast_route(previous, family));
            // IPv6 validates a global gateway against this table first; a
            // terminal-only table rejects it, so lift the terminal briefly.
            // The transaction guard still blocks new unmatched traffic.
            let gateway_needs_open_table = family == IpFamily::V6
                && previous_lane.is_some()
                && previous_unicast.is_none()
                && matches!(
                    &unicast,
                    Some(OwnedRoute::Unicast {
                        gateway: Some(_),
                        ..
                    })
                );
            if gateway_needs_open_table {
                steps.push(RoutingStep::DeleteRoute {
                    family,
                    table: lane.table,
                    route: OwnedRoute::Terminal,
                });
            }
            if let Some(route) = unicast {
                steps.push(RoutingStep::ReplaceRoute {
                    family,
                    table: lane.table,
                    route,
                });
            }
            steps.push(RoutingStep::ReplaceRoute {
                family,
                table: lane.table,
                route: OwnedRoute::Terminal,
            });
        }
    }
    let previous_rules = previous.map(install_rules).unwrap_or_default();
    steps.extend(
        install_rules(next)
            .into_iter()
            .filter(|rule| !previous_rules.contains(rule)),
    );
    steps
}

fn install_rules(plan: &KernelPlan) -> Vec<RoutingStep> {
    plan.lanes
        .iter()
        .flat_map(|lane| FAMILIES.map(|family| rule_step(lane, family)))
        .collect()
}

/// Steps that remove what `previous` owned and `next` does not, after the new
/// generation is verified. Removals tolerate resources the kernel already dropped.
pub fn retire_steps(previous: &KernelPlan, next: &KernelPlan) -> Vec<RoutingStep> {
    let mut steps = Vec::new();
    let next_rules = install_rules(next);
    for rule in install_rules(previous) {
        if !next_rules.contains(&rule) {
            let RoutingStep::AddRule {
                family,
                priority,
                table,
                mark,
            } = rule
            else {
                unreachable!("install_rules yields rule steps");
            };
            steps.push(RoutingStep::DeleteRule {
                family,
                priority,
                table,
                mark,
            });
        }
    }
    for lane in &previous.lanes {
        let Some(next_lane) = lane_for_table(next, lane.table) else {
            for family in FAMILIES {
                steps.push(RoutingStep::FlushTable {
                    family,
                    table: lane.table,
                });
            }
            continue;
        };
        for family in FAMILIES {
            if let Some(route) = lane_unicast_route(lane, family) {
                if lane_unicast_route(next_lane, family).is_none() {
                    steps.push(RoutingStep::DeleteRoute {
                        family,
                        table: lane.table,
                        route,
                    });
                }
            }
        }
    }
    steps
}

pub fn build_policy_commands(plan: &KernelPlan) -> Vec<CommandSpec> {
    install_steps(plan)
        .iter()
        .map(RoutingStep::command)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use crate::domain::{LaneFailurePolicy, LaneId, ProfileId};

    use super::{build_policy_commands, CommandSpec};
    use crate::backend::{KernelPlan, LaneAvailability, LanePlan, RouteMode};

    fn lane(
        id: &str,
        interface: &str,
        gateways: (Option<Ipv4Addr>, Option<Ipv6Addr>),
        availability: LaneAvailability,
        resources: (u32, u32, u32),
    ) -> LanePlan {
        LanePlan {
            identity: None,
            source_v4: vec![],
            source_v6: vec![],
            id: LaneId::from(id),
            interface: interface.to_owned(),
            route_v4: gateways.0.map(RouteMode::Via),
            route_v6: gateways.1.map(RouteMode::Via),
            availability,
            table: resources.0,
            mark: resources.1,
            rule_priority: resources.2,
            failure_policy: LaneFailurePolicy::Inactive,
        }
    }

    fn plan(lanes: Vec<LanePlan>) -> KernelPlan {
        KernelPlan {
            primary: Default::default(),
            local_networks: vec![],
            local_addresses: vec![],
            generation: 1,
            profile_id: ProfileId::from("default"),
            lanes,
            rules: vec![],
        }
    }

    fn terminal(family: &str, table: &str) -> CommandSpec {
        let args = [
            "route",
            "replace",
            "unreachable",
            "default",
            "table",
            table,
            "metric",
            "4294967294",
        ];
        if family == "4" {
            CommandSpec::ip4(args)
        } else {
            CommandSpec::ip6(args)
        }
    }

    fn rule(family: &str, mark: &str, table: &str, priority: &str) -> CommandSpec {
        let args = [
            "rule", "add", "fwmark", mark, "lookup", table, "priority", priority,
        ];
        if family == "4" {
            CommandSpec::ip4(args)
        } else {
            CommandSpec::ip6(args)
        }
    }

    #[test]
    fn builds_all_routes_before_policy_rules_in_lane_order() {
        let commands = build_policy_commands(&plan(vec![
            lane(
                "work",
                "wg0",
                (
                    Some("10.0.0.1".parse().unwrap()),
                    Some("2001:db8::1".parse().unwrap()),
                ),
                LaneAvailability::Available,
                (20_000, 0x524c0001, 10_000),
            ),
            lane(
                "privacy",
                "wg1",
                (Some("10.1.0.1".parse().unwrap()), None),
                LaneAvailability::Available,
                (20_001, 0x524c0002, 10_001),
            ),
        ]));

        assert_eq!(
            commands,
            vec![
                CommandSpec::ip4([
                    "route", "replace", "default", "via", "10.0.0.1", "dev", "wg0", "table",
                    "20000",
                ]),
                terminal("4", "20000"),
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
                terminal("6", "20000"),
                CommandSpec::ip4([
                    "route", "replace", "default", "via", "10.1.0.1", "dev", "wg1", "table",
                    "20001",
                ]),
                terminal("4", "20001"),
                terminal("6", "20001"),
                rule("4", "0x524c0001", "20000", "10000"),
                rule("6", "0x524c0001", "20000", "10000"),
                rule("4", "0x524c0002", "20001", "10001"),
                rule("6", "0x524c0002", "20001", "10001"),
            ]
        );
    }

    #[test]
    fn unavailable_lanes_and_missing_families_own_only_terminal_routes() {
        let commands = build_policy_commands(&plan(vec![
            lane(
                "offline",
                "wg0",
                (
                    Some("10.0.0.1".parse().unwrap()),
                    Some("2001:db8::1".parse().unwrap()),
                ),
                LaneAvailability::Unavailable,
                (20_000, 0x524c0001, 10_000),
            ),
            lane(
                "v6-only",
                "wg1",
                (None, Some("2001:db8:1::1".parse().unwrap())),
                LaneAvailability::Available,
                (20_001, 0x524c0002, 10_001),
            ),
        ]));

        assert_eq!(
            commands,
            vec![
                terminal("4", "20000"),
                terminal("6", "20000"),
                terminal("4", "20001"),
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
                terminal("6", "20001"),
                rule("4", "0x524c0001", "20000", "10000"),
                rule("6", "0x524c0001", "20000", "10000"),
                rule("4", "0x524c0002", "20001", "10001"),
                rule("6", "0x524c0002", "20001", "10001"),
            ]
        );
    }

    #[test]
    fn builds_dual_stack_gatewayless_default_routes_and_rules() {
        let mut gatewayless = lane(
            "wireguard",
            "wg0",
            (None, None),
            LaneAvailability::Available,
            (20_000, 0x524c0001, 10_000),
        );
        gatewayless.route_v4 = Some(RouteMode::Device);
        gatewayless.route_v6 = Some(RouteMode::Device);

        assert_eq!(
            build_policy_commands(&plan(vec![gatewayless])),
            vec![
                CommandSpec::ip4(["route", "replace", "default", "dev", "wg0", "table", "20000",]),
                terminal("4", "20000"),
                CommandSpec::ip6(["route", "replace", "default", "dev", "wg0", "table", "20000",]),
                terminal("6", "20000"),
                rule("4", "0x524c0001", "20000", "10000"),
                rule("6", "0x524c0001", "20000", "10000"),
            ]
        );
    }

    #[test]
    fn ipv6_gateway_into_terminal_only_table_lifts_terminal_first() {
        use super::{transition_steps, OwnedRoute, RoutingStep};
        use crate::backend::IpFamily;
        let gateway = "2001:db8:1::1".parse::<Ipv6Addr>().unwrap();
        let previous = plan(vec![lane(
            "vinn",
            "usb0",
            (Some("10.1.0.1".parse().unwrap()), None),
            LaneAvailability::Available,
            (20_000, 0x524c0001, 10_000),
        )]);
        let mut next = previous.clone();
        next.lanes[0].route_v6 = Some(RouteMode::Via(gateway));
        let steps = transition_steps(Some(&previous), &next);
        let v6 = steps
            .iter()
            .filter(|step| {
                matches!(
                    step,
                    RoutingStep::ReplaceRoute {
                        family: IpFamily::V6,
                        ..
                    } | RoutingStep::DeleteRoute {
                        family: IpFamily::V6,
                        ..
                    }
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            v6,
            vec![
                RoutingStep::DeleteRoute {
                    family: IpFamily::V6,
                    table: 20_000,
                    route: OwnedRoute::Terminal,
                },
                RoutingStep::ReplaceRoute {
                    family: IpFamily::V6,
                    table: 20_000,
                    route: OwnedRoute::Unicast {
                        gateway: Some(gateway.into()),
                        interface: "usb0".to_owned(),
                    },
                },
                RoutingStep::ReplaceRoute {
                    family: IpFamily::V6,
                    table: 20_000,
                    route: OwnedRoute::Terminal,
                },
            ]
        );
        assert!(
            !steps
                .iter()
                .any(|step| matches!(step, RoutingStep::AddRule { .. })),
            "existing lane rules are reused, not duplicated"
        );
    }

    #[test]
    fn retiring_a_lost_family_deletes_only_its_unicast_and_keeps_terminal() {
        use super::{retire_steps, OwnedRoute, RoutingStep};
        use crate::backend::IpFamily;
        let previous = plan(vec![
            lane(
                "eth",
                "eth0",
                (Some("10.0.0.1".parse().unwrap()), None),
                LaneAvailability::Available,
                (20_000, 0x524c0001, 10_000),
            ),
            lane(
                "gone",
                "wg1",
                (Some("10.1.0.1".parse().unwrap()), None),
                LaneAvailability::Available,
                (20_001, 0x524c0002, 10_001),
            ),
        ]);
        let mut next = previous.clone();
        next.lanes.truncate(1);
        next.lanes[0].availability = LaneAvailability::Unavailable;
        assert_eq!(
            retire_steps(&previous, &next),
            vec![
                RoutingStep::DeleteRule {
                    family: IpFamily::V4,
                    priority: 10_001,
                    table: 20_001,
                    mark: 0x524c0002,
                },
                RoutingStep::DeleteRule {
                    family: IpFamily::V6,
                    priority: 10_001,
                    table: 20_001,
                    mark: 0x524c0002,
                },
                RoutingStep::DeleteRoute {
                    family: IpFamily::V4,
                    table: 20_000,
                    route: OwnedRoute::Unicast {
                        gateway: Some("10.0.0.1".parse::<Ipv4Addr>().unwrap().into()),
                        interface: "eth0".to_owned(),
                    },
                },
                RoutingStep::FlushTable {
                    family: IpFamily::V4,
                    table: 20_001,
                },
                RoutingStep::FlushTable {
                    family: IpFamily::V6,
                    table: 20_001,
                },
            ]
        );
        assert_eq!(
            retire_steps(&previous, &next)[2].command().args,
            vec![
                "route", "del", "default", "via", "10.0.0.1", "dev", "eth0", "table", "20000",
                "metric", "0",
            ]
        );
    }
}
