use std::collections::BTreeSet;
use std::fmt;
use std::net::IpAddr;

use serde::Deserialize;

use super::{KernelPlan, LaneAvailability};
use crate::backend::IpFamily;

#[derive(Debug, Clone, Copy)]
pub struct SnapshotInput<'a> {
    pub ipv4_routes: &'a [u8],
    pub ipv6_routes: &'a [u8],
    pub ipv4_rules: &'a [u8],
    pub ipv6_rules: &'a [u8],
    pub nft: &'a [u8],
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KernelSnapshot {
    routes: Vec<RouteEntry>,
    rules: Vec<RuleEntry>,
    nft_tables: BTreeSet<(String, String)>,
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
        })
    }

    pub fn has_nft_table(&self, family: &str, name: &str) -> bool {
        self.nft_tables
            .contains(&(family.to_owned(), name.to_owned()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RouteEntry {
    family: IpFamily,
    table: TableRef,
    route_type: Option<String>,
    destination: Option<String>,
    gateway: Option<IpAddr>,
    interface: Option<String>,
    metric: Option<u32>,
    extra: BTreeSet<String>,
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
            && self.table.number() == Some(table)
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
            && self.extra.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuleEntry {
    family: IpFamily,
    priority: u32,
    table: TableRef,
    mark: Option<Mark>,
    source: Option<String>,
    destination: Option<String>,
    extra: BTreeSet<String>,
}

impl RuleEntry {
    fn matches_policy(&self, family: IpFamily, priority: u32, table: u32, mark: u32) -> bool {
        self.family == family
            && self.priority == priority
            && self.table.number() == Some(table)
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
    fn number(&self) -> Option<u32> {
        match self {
            Self::Number(number) => Some(*number),
            Self::Name(name) => match name.as_str() {
                "default" => Some(253),
                "main" => Some(254),
                "local" => Some(255),
                _ => name.parse().ok(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    table: TableRef,
    #[serde(default, alias = "mark")]
    fwmark: Option<MarkValue>,
    #[serde(default)]
    src: Option<String>,
    #[serde(default)]
    dst: Option<String>,
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, serde_json::Value>,
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
            Ok(RouteEntry {
                family,
                table: route.table,
                route_type: route.route_type,
                destination: route.dst,
                gateway,
                interface: route.dev,
                metric: route.metric,
                extra: route.extra.into_keys().collect(),
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
                table: rule.table,
                mark: rule
                    .fwmark
                    .map(|value| parse_mark(value, source))
                    .transpose()?,
                source: rule.src,
                destination: rule.dst,
                extra: rule.extra.into_keys().collect(),
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

pub fn detect_resource_conflicts(
    plan: &KernelPlan,
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
            )?;
        }
        if let Some(gateway) = lane.gateway_v6 {
            check_route(
                snapshot,
                IpFamily::V6,
                lane.table,
                IpAddr::V6(gateway),
                &lane.interface,
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
            )?;
        }
        if lane.gateway_v6.is_some() {
            check_rule(
                snapshot,
                IpFamily::V6,
                lane.rule_priority,
                lane.table,
                lane.mark,
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
) -> Result<(), ResourceConflict> {
    if snapshot.routes.iter().any(|route| {
        route.family == family
            && route.table.number() == Some(table)
            && !route.matches_default(family, table, gateway, interface)
    }) {
        Err(ResourceConflict::RoutingTableInUse(table))
    } else {
        Ok(())
    }
}

fn check_rule(
    snapshot: &KernelSnapshot,
    family: IpFamily,
    priority: u32,
    table: u32,
    mark: u32,
) -> Result<(), ResourceConflict> {
    if snapshot.rules.iter().any(|rule| {
        rule.family == family
            && rule.priority == priority
            && !rule.matches_policy(family, priority, table, mark)
    }) {
        Err(ResourceConflict::RulePriorityInUse(priority))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::backend::{
        detect_resource_conflicts, KernelPlan, LaneAvailability, LanePlan, ResourceConflict,
        SnapshotInput, SnapshotSource,
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
}
