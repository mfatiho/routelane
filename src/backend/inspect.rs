use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use super::{
    CommandSpec, IpFamily, KernelPlan, LaneAvailability, PlanHash, Program, TopologySnapshot,
};

#[derive(Debug, Clone, Copy)]
pub struct SnapshotInput<'a> {
    pub ipv4_routes: &'a [u8],
    pub ipv6_routes: &'a [u8],
    pub ipv4_rules: &'a [u8],
    pub ipv6_rules: &'a [u8],
    pub nft: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCommands {
    pub ipv4_routes: CommandSpec,
    pub ipv6_routes: CommandSpec,
    pub ipv4_rules: CommandSpec,
    pub ipv6_rules: CommandSpec,
    pub nft: CommandSpec,
}

pub fn build_snapshot_commands() -> SnapshotCommands {
    SnapshotCommands {
        ipv4_routes: CommandSpec::ip4(["-j", "-N", "route", "show", "table", "all"]),
        ipv6_routes: CommandSpec::ip6(["-j", "-N", "route", "show", "table", "all"]),
        ipv4_rules: CommandSpec::ip4(["-j", "-N", "rule", "show"]),
        ipv6_rules: CommandSpec::ip6(["-j", "-N", "rule", "show"]),
        nft: CommandSpec {
            program: Program::Nft,
            family: None,
            args: ["-j", "list", "table", "inet", "routelane"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            stdin: None,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotSource {
    Ipv4Routes,
    Ipv6Routes,
    Ipv4Rules,
    Ipv6Rules,
    Nft,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotParseError {
    pub source: SnapshotSource,
    pub message: String,
}

impl fmt::Display for SnapshotParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to parse {} snapshot: {}",
            self.source, self.message
        )
    }
}

impl std::error::Error for SnapshotParseError {}

impl fmt::Display for SnapshotSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Ipv4Routes => "IPv4 routes",
            Self::Ipv6Routes => "IPv6 routes",
            Self::Ipv4Rules => "IPv4 rules",
            Self::Ipv6Rules => "IPv6 rules",
            Self::Nft => "nftables",
        };
        formatter.write_str(label)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct KernelSnapshot {
    routes: Vec<RouteEntry>,
    rules: Vec<RuleEntry>,
    nft_tables: BTreeSet<(String, String)>,
    topology: TopologySnapshot,
    nft_generation: Option<u64>,
    nft_plan_hash: Option<PlanHash>,
}

impl KernelSnapshot {
    pub fn parse(input: SnapshotInput<'_>) -> Result<Self, SnapshotParseError> {
        let mut routes = parse_routes(input.ipv4_routes, IpFamily::V4, SnapshotSource::Ipv4Routes)?;
        routes.extend(parse_routes(
            input.ipv6_routes,
            IpFamily::V6,
            SnapshotSource::Ipv6Routes,
        )?);

        let mut rules = parse_rules(input.ipv4_rules, IpFamily::V4, SnapshotSource::Ipv4Rules)?;
        rules.extend(parse_rules(
            input.ipv6_rules,
            IpFamily::V6,
            SnapshotSource::Ipv6Rules,
        )?);

        Ok(Self {
            routes,
            rules,
            nft_tables: parse_nft_tables(input.nft)?,
            topology: TopologySnapshot::empty(),
            nft_generation: None,
            nft_plan_hash: None,
        })
    }

    pub fn with_topology(mut self, topology: &TopologySnapshot) -> Self {
        self.topology = topology.clone();
        self
    }

    pub fn with_nft_ownership(mut self, generation: u64, plan_hash: PlanHash) -> Self {
        self.nft_generation = Some(generation);
        self.nft_plan_hash = Some(plan_hash);
        self
    }

    pub fn has_nft_table(&self, family: &str, name: &str) -> bool {
        self.nft_tables
            .contains(&(family.to_owned(), name.to_owned()))
    }

    pub fn has_interface(&self, interface: &str) -> bool {
        self.topology.interface(interface).is_some()
    }

    pub fn gateway_reachable(&self, interface: &str, gateway: IpAddr) -> bool {
        let Some(topology) = self.topology.interface(interface) else {
            return false;
        };
        match gateway {
            IpAddr::V4(address) => topology.gateway_v4 == Some(address),
            IpAddr::V6(address) => topology.gateway_v6 == Some(address),
        }
    }

    pub fn nft_is_owned_by(&self, plan: &KernelPlan) -> bool {
        self.has_nft_table("inet", "routelane")
            && self.nft_generation == Some(plan.generation)
            && self.nft_plan_hash == Some(plan.content_hash())
    }

    pub fn route_state(
        &self,
        family: IpFamily,
        table: u32,
        gateway: IpAddr,
        interface: &str,
    ) -> ResourceState {
        let mut entries = self
            .routes
            .iter()
            .filter(|route| route.family == family && route.table == table);
        match (entries.next(), entries.next()) {
            (None, None) => ResourceState::Missing,
            (Some(route), None) if route.matches_default(family, table, gateway, interface) => {
                ResourceState::Exact
            }
            _ => ResourceState::Foreign,
        }
    }

    pub fn rule_state(
        &self,
        family: IpFamily,
        priority: u32,
        table: u32,
        mark: u32,
    ) -> ResourceState {
        let mut entries = self
            .rules
            .iter()
            .filter(|rule| rule.family == family && rule.priority == priority);
        match (entries.next(), entries.next()) {
            (None, None) => ResourceState::Missing,
            (Some(rule), None) if rule.matches_policy(family, priority, table, mark) => {
                ResourceState::Exact
            }
            _ => ResourceState::Foreign,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RouteEntry {
    family: IpFamily,
    table: u32,
    route_type: Option<String>,
    destination: Option<String>,
    gateway: Option<IpAddr>,
    interface: Option<String>,
    metric: Option<u32>,
    protocol: Option<String>,
    scope: Option<String>,
    preferred_source: Option<IpAddr>,
    flags: Vec<String>,
    preference: Option<String>,
    extra: BTreeMap<String, serde_json::Value>,
}

impl RouteEntry {
    fn matches_default(
        &self,
        family: IpFamily,
        table: u32,
        gateway: IpAddr,
        interface: &str,
    ) -> bool {
        self.family == family
            && self.table == table
            && self.destination.as_deref() == Some("default")
            && self.gateway == Some(gateway)
            && self.interface.as_deref() == Some(interface)
            && self
                .route_type
                .as_deref()
                .is_none_or(|route_type| route_type == "unicast")
            && self.metric.unwrap_or_default()
                == match family {
                    IpFamily::V4 => 0,
                    IpFamily::V6 => 1024,
                }
            && self
                .protocol
                .as_deref()
                .is_none_or(|protocol| protocol == "boot")
            && self.scope.as_deref().is_none_or(|scope| scope == "global")
            && self.preferred_source.is_none()
            && self.flags.is_empty()
            && match family {
                IpFamily::V4 => self.preference.is_none(),
                IpFamily::V6 => self
                    .preference
                    .as_deref()
                    .is_none_or(|preference| preference == "medium"),
            }
            && self.extra.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RuleEntry {
    family: IpFamily,
    priority: u32,
    table: Option<u32>,
    mark: Option<Mark>,
    source: Option<String>,
    destination: Option<String>,
    extra: BTreeMap<String, serde_json::Value>,
}

impl RuleEntry {
    fn matches_policy(&self, family: IpFamily, priority: u32, table: u32, mark: u32) -> bool {
        self.family == family
            && self.priority == priority
            && self.table == Some(table)
            && self
                .mark
                .as_ref()
                .is_some_and(|existing| existing.value == mark && existing.is_full_width())
            && self.source.as_deref().is_none_or(|source| source == "all")
            && self
                .destination
                .as_deref()
                .is_none_or(|destination| destination == "all")
            && self.extra.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
enum TableRef {
    Number(u32),
    Name(String),
}

impl Default for TableRef {
    fn default() -> Self {
        Self::Number(254)
    }
}

impl TableRef {
    fn into_number(self, source: SnapshotSource) -> Result<u32, SnapshotParseError> {
        match self {
            Self::Number(number) => Ok(number),
            Self::Name(name) => match name.as_str() {
                "default" => Ok(253),
                "main" => Ok(254),
                "local" => Ok(255),
                _ => name.parse().map_err(|_| {
                    parse_error(source, format!("unsupported symbolic table name {name:?}"))
                }),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Mark {
    value: u32,
    mask: Option<u32>,
}

impl Mark {
    fn is_full_width(&self) -> bool {
        self.mask.is_none() || self.mask == Some(u32::MAX)
    }
}

#[derive(Debug, Deserialize)]
struct RawRoute {
    #[serde(default, rename = "type")]
    route_type: Option<String>,
    #[serde(default)]
    dst: Option<String>,
    #[serde(default)]
    gateway: Option<String>,
    #[serde(default)]
    dev: Option<String>,
    #[serde(default)]
    table: TableRef,
    #[serde(default)]
    metric: Option<u32>,
    #[serde(default)]
    protocol: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    prefsrc: Option<String>,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    pref: Option<String>,
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct RawRule {
    priority: u32,
    #[serde(default)]
    table: Option<TableRef>,
    #[serde(default, alias = "mark")]
    fwmark: Option<MarkValue>,
    #[serde(default)]
    src: Option<String>,
    #[serde(default)]
    dst: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MarkValue {
    Number(u32),
    Text(String),
}

#[derive(Debug, Deserialize)]
struct NftEnvelope {
    nftables: Vec<NftObject>,
}

#[derive(Debug, Deserialize)]
struct NftObject {
    #[serde(default)]
    table: Option<NftTable>,
}

#[derive(Debug, Deserialize)]
struct NftTable {
    family: String,
    name: String,
}

fn parse_routes(
    json: &[u8],
    family: IpFamily,
    source: SnapshotSource,
) -> Result<Vec<RouteEntry>, SnapshotParseError> {
    let raw: Vec<RawRoute> = deserialize(json, source)?;
    raw.into_iter()
        .map(|route| {
            let gateway = route
                .gateway
                .map(|value| {
                    value.parse::<IpAddr>().map_err(|error| {
                        parse_error(source, format!("invalid gateway {value:?}: {error}"))
                    })
                })
                .transpose()?;
            if gateway.is_some_and(|address| !family_matches(family, address)) {
                return Err(parse_error(
                    source,
                    "gateway address does not match route family",
                ));
            }
            let preferred_source = route
                .prefsrc
                .map(|value| {
                    value.parse::<IpAddr>().map_err(|error| {
                        parse_error(
                            source,
                            format!("invalid preferred source {value:?}: {error}"),
                        )
                    })
                })
                .transpose()?;
            if preferred_source.is_some_and(|address| !family_matches(family, address)) {
                return Err(parse_error(
                    source,
                    "preferred source address does not match route family",
                ));
            }
            Ok(RouteEntry {
                family,
                table: route.table.into_number(source)?,
                route_type: route.route_type,
                destination: route.dst,
                gateway,
                interface: route.dev,
                metric: route.metric,
                protocol: route.protocol,
                scope: route.scope,
                preferred_source,
                flags: route.flags,
                preference: route.pref,
                extra: route.extra,
            })
        })
        .collect()
}

fn parse_rules(
    json: &[u8],
    family: IpFamily,
    source: SnapshotSource,
) -> Result<Vec<RuleEntry>, SnapshotParseError> {
    let raw: Vec<RawRule> = deserialize(json, source)?;
    raw.into_iter()
        .map(|rule| {
            Ok(RuleEntry {
                family,
                priority: rule.priority,
                table: rule
                    .table
                    .map(|table| table.into_number(source))
                    .transpose()?,
                mark: rule
                    .fwmark
                    .map(|value| parse_mark(value, source))
                    .transpose()?,
                source: rule.src,
                destination: rule.dst,
                extra: rule.extra,
            })
        })
        .collect()
}

fn parse_nft_tables(json: &[u8]) -> Result<BTreeSet<(String, String)>, SnapshotParseError> {
    let envelope: NftEnvelope = deserialize(json, SnapshotSource::Nft)?;
    Ok(envelope
        .nftables
        .into_iter()
        .filter_map(|object| object.table)
        .map(|table| (table.family, table.name))
        .collect())
}

fn deserialize<'a, T: Deserialize<'a>>(
    json: &'a [u8],
    source: SnapshotSource,
) -> Result<T, SnapshotParseError> {
    serde_json::from_slice(json).map_err(|error| parse_error(source, error.to_string()))
}

fn parse_mark(value: MarkValue, source: SnapshotSource) -> Result<Mark, SnapshotParseError> {
    match value {
        MarkValue::Number(value) => Ok(Mark { value, mask: None }),
        MarkValue::Text(value) => {
            let (mark, mask) = value
                .split_once('/')
                .map_or((value.as_str(), None), |(mark, mask)| (mark, Some(mask)));
            Ok(Mark {
                value: parse_u32(mark)
                    .map_err(|message| parse_error(source, format!("invalid fwmark: {message}")))?,
                mask: mask
                    .map(parse_u32)
                    .transpose()
                    .map_err(|message| parse_error(source, format!("invalid fwmark: {message}")))?,
            })
        }
    }
}

fn parse_u32(value: &str) -> Result<u32, String> {
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).map_err(|error| error.to_string())
    } else {
        value.parse::<u32>().map_err(|error| error.to_string())
    }
}

fn family_matches(family: IpFamily, address: IpAddr) -> bool {
    matches!(
        (family, address),
        (IpFamily::V4, IpAddr::V4(_)) | (IpFamily::V6, IpAddr::V6(_))
    )
}

fn parse_error(source: SnapshotSource, message: impl Into<String>) -> SnapshotParseError {
    SnapshotParseError {
        source,
        message: message.into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceConflict {
    RoutingTableInUse(u32),
    RulePriorityInUse(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceState {
    Missing,
    Exact,
    Foreign,
}

pub fn detect_resource_conflicts(
    plan: &KernelPlan,
    snapshot: &KernelSnapshot,
) -> Result<(), ResourceConflict> {
    detect_resource_conflicts_with_owner(plan, None, snapshot)
}

pub fn detect_resource_conflicts_for_update(
    next: &KernelPlan,
    current: &KernelPlan,
    snapshot: &KernelSnapshot,
) -> Result<(), ResourceConflict> {
    detect_resource_conflicts_with_owner(next, Some(current), snapshot)
}

fn detect_resource_conflicts_with_owner(
    plan: &KernelPlan,
    current: Option<&KernelPlan>,
    snapshot: &KernelSnapshot,
) -> Result<(), ResourceConflict> {
    let lanes = plan
        .lanes
        .iter()
        .filter(|lane| lane.availability == LaneAvailability::Available);

    for lane in lanes.clone() {
        if let Some(gateway) = lane.gateway_v4 {
            check_route(
                snapshot,
                IpFamily::V4,
                lane.table,
                IpAddr::V4(gateway),
                &lane.interface,
                current,
            )?;
        }
        if let Some(gateway) = lane.gateway_v6 {
            check_route(
                snapshot,
                IpFamily::V6,
                lane.table,
                IpAddr::V6(gateway),
                &lane.interface,
                current,
            )?;
        }
    }

    for lane in lanes {
        if lane.gateway_v4.is_some() {
            check_rule(
                snapshot,
                IpFamily::V4,
                lane.rule_priority,
                lane.table,
                lane.mark,
                current,
            )?;
        }
        if lane.gateway_v6.is_some() {
            check_rule(
                snapshot,
                IpFamily::V6,
                lane.rule_priority,
                lane.table,
                lane.mark,
                current,
            )?;
        }
    }

    Ok(())
}

fn check_route(
    snapshot: &KernelSnapshot,
    family: IpFamily,
    table: u32,
    gateway: IpAddr,
    interface: &str,
    current: Option<&KernelPlan>,
) -> Result<(), ResourceConflict> {
    if let Some(current) = current {
        if let Some((owned_gateway, owned_interface)) = owned_route(current, family, table)
            .map_err(|()| ResourceConflict::RoutingTableInUse(table))?
        {
            let mut entries = snapshot
                .routes
                .iter()
                .filter(|route| route.family == family && route.table == table);
            return match (entries.next(), entries.next()) {
                (None, None) => Ok(()),
                (Some(route), None)
                    if route.matches_default(family, table, owned_gateway, &owned_interface) =>
                {
                    Ok(())
                }
                _ => Err(ResourceConflict::RoutingTableInUse(table)),
            };
        }
    }

    let mut entries = snapshot
        .routes
        .iter()
        .filter(|route| route.family == family && route.table == table);
    match (entries.next(), entries.next()) {
        (None, None) => Ok(()),
        (Some(route), None) if route.matches_default(family, table, gateway, interface) => Ok(()),
        _ => Err(ResourceConflict::RoutingTableInUse(table)),
    }
}

fn check_rule(
    snapshot: &KernelSnapshot,
    family: IpFamily,
    priority: u32,
    table: u32,
    mark: u32,
    current: Option<&KernelPlan>,
) -> Result<(), ResourceConflict> {
    if let Some(current) = current {
        if let Some((owned_table, owned_mark)) = owned_rule(current, family, priority)
            .map_err(|()| ResourceConflict::RulePriorityInUse(priority))?
        {
            let mut entries = snapshot
                .rules
                .iter()
                .filter(|rule| rule.family == family && rule.priority == priority);
            return match (entries.next(), entries.next()) {
                (None, None) => Ok(()),
                (Some(rule), None)
                    if rule.matches_policy(family, priority, owned_table, owned_mark) =>
                {
                    Ok(())
                }
                _ => Err(ResourceConflict::RulePriorityInUse(priority)),
            };
        }
    }

    let mut entries = snapshot
        .rules
        .iter()
        .filter(|rule| rule.family == family && rule.priority == priority);
    match (entries.next(), entries.next()) {
        (None, None) => Ok(()),
        (Some(rule), None) if rule.matches_policy(family, priority, table, mark) => Ok(()),
        _ => Err(ResourceConflict::RulePriorityInUse(priority)),
    }
}

fn owned_route(
    current: &KernelPlan,
    family: IpFamily,
    table: u32,
) -> Result<Option<(IpAddr, String)>, ()> {
    exactly_one(current.lanes.iter().filter_map(|lane| {
        if lane.availability != LaneAvailability::Available || lane.table != table {
            return None;
        }
        let gateway = match family {
            IpFamily::V4 => lane.gateway_v4.map(IpAddr::V4),
            IpFamily::V6 => lane.gateway_v6.map(IpAddr::V6),
        }?;
        Some((gateway, lane.interface.clone()))
    }))
}

fn owned_rule(
    current: &KernelPlan,
    family: IpFamily,
    priority: u32,
) -> Result<Option<(u32, u32)>, ()> {
    exactly_one(current.lanes.iter().filter_map(|lane| {
        if lane.availability != LaneAvailability::Available || lane.rule_priority != priority {
            return None;
        }
        let family_is_installed = match family {
            IpFamily::V4 => lane.gateway_v4.is_some(),
            IpFamily::V6 => lane.gateway_v6.is_some(),
        };
        family_is_installed.then_some((lane.table, lane.mark))
    }))
}

fn exactly_one<T>(mut values: impl Iterator<Item = T>) -> Result<Option<T>, ()> {
    let first = values.next();
    if values.next().is_some() {
        Err(())
    } else {
        Ok(first)
    }
}

#[cfg(test)]
mod tests {
    use crate::backend::{
        build_snapshot_commands, detect_resource_conflicts, CommandSpec, IpFamily, KernelPlan,
        LaneAvailability, LanePlan, Program, ResourceConflict, SnapshotInput, SnapshotSource,
    };
    use crate::domain::{LaneFailurePolicy, LaneId, ProfileId};

    use super::KernelSnapshot;

    const EMPTY: &[u8] = b"[]";
    const EMPTY_NFT: &[u8] = br#"{"nftables":[{"metainfo":{"json_schema_version":1}}]}"#;

    fn plan() -> KernelPlan {
        KernelPlan {
            generation: 1,
            profile_id: ProfileId::from("default"),
            lanes: vec![LanePlan {
                id: LaneId::from("work"),
                interface: "wg0".to_owned(),
                gateway_v4: Some("10.0.0.1".parse().unwrap()),
                gateway_v6: Some("2001:db8::1".parse().unwrap()),
                availability: LaneAvailability::Available,
                table: 20_000,
                mark: 0x524c0001,
                rule_priority: 10_000,
                failure_policy: LaneFailurePolicy::Inactive,
            }],
            rules: vec![],
        }
    }

    fn parse(
        ipv4_routes: &[u8],
        ipv6_routes: &[u8],
        ipv4_rules: &[u8],
        ipv6_rules: &[u8],
        nft: &[u8],
    ) -> KernelSnapshot {
        KernelSnapshot::parse(SnapshotInput {
            ipv4_routes,
            ipv6_routes,
            ipv4_rules,
            ipv6_rules,
            nft,
        })
        .unwrap()
    }

    #[test]
    fn parses_real_iproute2_field_variants_and_nft_envelope() {
        let snapshot = parse(
            br#"[{"dst":"default","gateway":"10.0.0.1","dev":"wg0","table":"20000","flags":[]}]"#,
            br#"[{"dst":"default","gateway":"2001:db8::1","dev":"wg0","table":20000,"metric":1024,"flags":[],"pref":"medium"}]"#,
            br#"[{"priority":10000,"src":"all","fwmark":"0x524c0001","table":"20000"}]"#,
            br#"[{"priority":10000,"src":"all","fwmark":1380712449,"table":20000}]"#,
            br#"{"nftables":[
                {"metainfo":{"version":"1.0.9","release_name":"Old Doc Yak","json_schema_version":1}},
                {"table":{"family":"inet","name":"routelane","handle":7}},
                {"chain":{"family":"inet","table":"routelane","name":"output","handle":8}},
                {"rule":{"family":"inet","table":"routelane","chain":"output","expr":[],"handle":9}}
            ]}"#,
        );

        assert!(snapshot.has_nft_table("inet", "routelane"));
        assert_eq!(detect_resource_conflicts(&plan(), &snapshot), Ok(()));
    }

    #[test]
    fn reports_the_input_source_for_malformed_json_instead_of_empty_state() {
        let cases = [
            (
                SnapshotSource::Ipv4Routes,
                SnapshotInput {
                    ipv4_routes: b"[",
                    ipv6_routes: EMPTY,
                    ipv4_rules: EMPTY,
                    ipv6_rules: EMPTY,
                    nft: EMPTY_NFT,
                },
            ),
            (
                SnapshotSource::Ipv6Routes,
                SnapshotInput {
                    ipv4_routes: EMPTY,
                    ipv6_routes: b"[",
                    ipv4_rules: EMPTY,
                    ipv6_rules: EMPTY,
                    nft: EMPTY_NFT,
                },
            ),
            (
                SnapshotSource::Ipv4Rules,
                SnapshotInput {
                    ipv4_routes: EMPTY,
                    ipv6_routes: EMPTY,
                    ipv4_rules: b"[",
                    ipv6_rules: EMPTY,
                    nft: EMPTY_NFT,
                },
            ),
            (
                SnapshotSource::Ipv6Rules,
                SnapshotInput {
                    ipv4_routes: EMPTY,
                    ipv6_routes: EMPTY,
                    ipv4_rules: EMPTY,
                    ipv6_rules: b"[",
                    nft: EMPTY_NFT,
                },
            ),
            (
                SnapshotSource::Nft,
                SnapshotInput {
                    ipv4_routes: EMPTY,
                    ipv6_routes: EMPTY,
                    ipv4_rules: EMPTY,
                    ipv6_rules: EMPTY,
                    nft: b"{",
                },
            ),
        ];

        for (expected_source, input) in cases {
            let error = KernelSnapshot::parse(input).unwrap_err();
            assert_eq!(error.source, expected_source);
            assert!(!error.message.is_empty());
        }
    }

    #[test]
    fn refuses_foreign_route_in_a_requested_table() {
        let snapshot = parse(
            br#"[{"dst":"default","gateway":"192.0.2.1","dev":"eth9","table":"20000","flags":[]}]"#,
            EMPTY,
            EMPTY,
            EMPTY,
            EMPTY_NFT,
        );

        assert_eq!(
            detect_resource_conflicts(&plan(), &snapshot),
            Err(ResourceConflict::RoutingTableInUse(20_000))
        );
    }

    #[test]
    fn refuses_foreign_rule_at_a_requested_priority() {
        let snapshot = parse(
            EMPTY,
            EMPTY,
            br#"[{"priority":10000,"src":"all","fwmark":"0x0000002a","table":"20000"}]"#,
            EMPTY,
            EMPTY_NFT,
        );

        assert_eq!(
            detect_resource_conflicts(&plan(), &snapshot),
            Err(ResourceConflict::RulePriorityInUse(10_000))
        );
    }

    #[test]
    fn ignores_state_outside_resources_requested_by_available_lane_families() {
        let snapshot = parse(
            br#"[{"dst":"default","gateway":"192.0.2.1","dev":"eth9","table":"20001","flags":[]}]"#,
            br#"[{"dst":"default","gateway":"2001:db8:ffff::1","dev":"eth9","table":"20001","flags":[]}]"#,
            br#"[
                {"priority":9999,"not":null,"src":"all","table":"main"},
                {"priority":10001,"src":"all","fwmark":"0x0000002a","table":"20001"}
            ]"#,
            br#"[{"priority":10001,"src":"all","fwmark":"0x0000002a","table":"20001"}]"#,
            EMPTY_NFT,
        );

        assert_eq!(detect_resource_conflicts(&plan(), &snapshot), Ok(()));
    }

    #[test]
    fn refuses_an_extra_route_even_when_the_owned_route_is_present() {
        let snapshot = parse(
            br#"[
                {"dst":"default","gateway":"10.0.0.1","dev":"wg0","table":"20000","flags":[]},
                {"dst":"192.0.2.0/24","dev":"wg0","table":"20000","flags":[]}
            ]"#,
            EMPTY,
            EMPTY,
            EMPTY,
            EMPTY_NFT,
        );

        assert_eq!(
            detect_resource_conflicts(&plan(), &snapshot),
            Err(ResourceConflict::RoutingTableInUse(20_000))
        );
    }

    #[test]
    fn refuses_route_with_same_next_hop_but_foreign_metric() {
        let snapshot = parse(
            br#"[{"dst":"default","gateway":"10.0.0.1","dev":"wg0","table":"20000","metric":50,"flags":[]}]"#,
            EMPTY,
            EMPTY,
            EMPTY,
            EMPTY_NFT,
        );

        assert_eq!(
            detect_resource_conflicts(&plan(), &snapshot),
            Err(ResourceConflict::RoutingTableInUse(20_000))
        );
    }

    #[test]
    fn refuses_rule_with_same_mark_and_table_but_foreign_selector() {
        let snapshot = parse(
            EMPTY,
            EMPTY,
            br#"[{"priority":10000,"src":"all","dst":"192.0.2.0/24","fwmark":"0x524c0001","table":"20000"}]"#,
            EMPTY,
            EMPTY_NFT,
        );

        assert_eq!(
            detect_resource_conflicts(&plan(), &snapshot),
            Err(ResourceConflict::RulePriorityInUse(10_000))
        );
    }

    #[test]
    fn parses_non_table_rule_actions_outside_requested_priorities() {
        let snapshot = parse(
            EMPTY,
            EMPTY,
            br#"[
                {"priority":9000,"src":"all","goto":10000},
                {"priority":9001,"src":"all","nop":null},
                {"priority":9002,"src":"all","action":"blackhole"}
            ]"#,
            EMPTY,
            EMPTY_NFT,
        );

        assert_eq!(detect_resource_conflicts(&plan(), &snapshot), Ok(()));
    }

    #[test]
    fn treats_non_table_rule_action_at_requested_priority_as_foreign() {
        let snapshot = parse(
            EMPTY,
            EMPTY,
            br#"[{"priority":10000,"src":"all","action":"blackhole"}]"#,
            EMPTY,
            EMPTY_NFT,
        );

        assert_eq!(
            detect_resource_conflicts(&plan(), &snapshot),
            Err(ResourceConflict::RulePriorityInUse(10_000))
        );
    }

    #[test]
    fn refuses_owned_route_shape_with_foreign_preferred_source() {
        let snapshot = parse(
            br#"[{
                "dst":"default",
                "gateway":"10.0.0.1",
                "dev":"wg0",
                "table":"20000",
                "prefsrc":"10.0.0.99",
                "flags":[]
            }]"#,
            EMPTY,
            EMPTY,
            EMPTY,
            EMPTY_NFT,
        );

        assert_eq!(
            detect_resource_conflicts(&plan(), &snapshot),
            Err(ResourceConflict::RoutingTableInUse(20_000))
        );
    }

    #[test]
    fn refuses_owned_route_shape_with_onlink_flag() {
        let snapshot = parse(
            br#"[{
                "dst":"default",
                "gateway":"10.0.0.1",
                "dev":"wg0",
                "table":"20000",
                "flags":["onlink"]
            }]"#,
            EMPTY,
            EMPTY,
            EMPTY,
            EMPTY_NFT,
        );

        assert_eq!(
            detect_resource_conflicts(&plan(), &snapshot),
            Err(ResourceConflict::RoutingTableInUse(20_000))
        );
    }

    #[test]
    fn refuses_non_default_route_protocol_scope_and_ipv6_preference() {
        let cases = [
            (
                br#"[{"dst":"default","gateway":"10.0.0.1","dev":"wg0","table":"20000","protocol":"dhcp","flags":[]}]"#
                    .as_slice(),
                EMPTY,
            ),
            (
                br#"[{"dst":"default","gateway":"10.0.0.1","dev":"wg0","table":"20000","scope":"link","flags":[]}]"#
                    .as_slice(),
                EMPTY,
            ),
            (
                EMPTY,
                br#"[{"dst":"default","gateway":"2001:db8::1","dev":"wg0","table":"20000","metric":1024,"flags":[],"pref":"high"}]"#
                    .as_slice(),
            ),
        ];

        for (ipv4_routes, ipv6_routes) in cases {
            let snapshot = parse(ipv4_routes, ipv6_routes, EMPTY, EMPTY, EMPTY_NFT);
            assert_eq!(
                detect_resource_conflicts(&plan(), &snapshot),
                Err(ResourceConflict::RoutingTableInUse(20_000))
            );
        }
    }

    #[test]
    fn refuses_duplicate_exact_route_in_requested_table() {
        let snapshot = parse(
            br#"[
                {"dst":"default","gateway":"10.0.0.1","dev":"wg0","table":"20000","flags":[]},
                {"dst":"default","gateway":"10.0.0.1","dev":"wg0","table":"20000","flags":[]}
            ]"#,
            EMPTY,
            EMPTY,
            EMPTY,
            EMPTY_NFT,
        );

        assert_eq!(
            detect_resource_conflicts(&plan(), &snapshot),
            Err(ResourceConflict::RoutingTableInUse(20_000))
        );
    }

    #[test]
    fn refuses_duplicate_exact_rule_at_requested_priority() {
        let snapshot = parse(
            EMPTY,
            EMPTY,
            br#"[
                {"priority":10000,"src":"all","fwmark":"0x524c0001","table":"20000"},
                {"priority":10000,"src":"all","fwmark":"0x524c0001","table":"20000"}
            ]"#,
            EMPTY,
            EMPTY_NFT,
        );

        assert_eq!(
            detect_resource_conflicts(&plan(), &snapshot),
            Err(ResourceConflict::RulePriorityInUse(10_000))
        );
    }

    #[test]
    fn reports_unknown_symbolic_table_names_for_routes_and_rules() {
        let cases = [
            (
                SnapshotSource::Ipv4Routes,
                SnapshotInput {
                    ipv4_routes:
                        br#"[{"dst":"default","gateway":"10.0.0.1","dev":"wg0","table":"vpn","flags":[]}]"#,
                    ipv6_routes: EMPTY,
                    ipv4_rules: EMPTY,
                    ipv6_rules: EMPTY,
                    nft: EMPTY_NFT,
                },
            ),
            (
                SnapshotSource::Ipv4Rules,
                SnapshotInput {
                    ipv4_routes: EMPTY,
                    ipv6_routes: EMPTY,
                    ipv4_rules:
                        br#"[{"priority":10000,"src":"all","fwmark":"0x524c0001","table":"vpn"}]"#,
                    ipv6_rules: EMPTY,
                    nft: EMPTY_NFT,
                },
            ),
        ];

        for (expected_source, input) in cases {
            let error = KernelSnapshot::parse(input).unwrap_err();
            assert_eq!(error.source, expected_source);
            assert!(error.message.contains("vpn"));
        }
    }

    #[test]
    fn snapshot_commands_request_json_with_numeric_table_ids() {
        let commands = build_snapshot_commands();

        assert_eq!(
            commands.ipv4_routes,
            CommandSpec::ip4(["-j", "-N", "route", "show", "table", "all"])
        );
        assert_eq!(
            commands.ipv6_routes,
            CommandSpec::ip6(["-j", "-N", "route", "show", "table", "all"])
        );
        assert_eq!(
            commands.ipv4_rules,
            CommandSpec::ip4(["-j", "-N", "rule", "show"])
        );
        assert_eq!(
            commands.ipv6_rules,
            CommandSpec::ip6(["-j", "-N", "rule", "show"])
        );
        assert_eq!(
            commands.nft,
            CommandSpec {
                program: Program::Nft,
                family: None,
                args: vec![
                    "-j".to_owned(),
                    "list".to_owned(),
                    "table".to_owned(),
                    "inet".to_owned(),
                    "routelane".to_owned(),
                ],
                stdin: None,
            }
        );
        assert_eq!(commands.ipv4_routes.family, Some(IpFamily::V4));
        assert_eq!(commands.ipv6_rules.family, Some(IpFamily::V6));
    }
}
