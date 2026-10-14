use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ipnet::IpNet;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use super::{IpFamily, RouteMode, MARK_BASE, RULE_PRIORITY_BASE, TABLE_BASE, TABLE_MAX};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceIdentity {
    pub ifindex: u32,
    pub name: String,
    pub kind: String,
    pub hardware_address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct InterfaceTopology {
    pub identity: Option<InterfaceIdentity>,
    pub link_ready: bool,
    pub route_v4: Option<RouteMode<Ipv4Addr>>,
    pub route_v6: Option<RouteMode<Ipv6Addr>>,
    pub source_v4: Vec<Ipv4Addr>,
    pub source_v6: Vec<Ipv6Addr>,
    pub connected_networks: Vec<IpNet>,
}

impl InterfaceTopology {
    pub fn ready(&self, family: IpFamily) -> bool {
        self.identity.is_some()
            && self.link_ready
            && match family {
                IpFamily::V4 => self.route_v4.is_some() && !self.source_v4.is_empty(),
                IpFamily::V6 => self.route_v6.is_some() && !self.source_v6.is_empty(),
            }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
enum DefaultSelection {
    #[default]
    None,
    Interface(String),
    Ambiguous(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TopologySnapshot {
    interfaces: BTreeMap<String, InterfaceTopology>,
    defaults: [DefaultSelection; 2],
    observed: bool,
    conflicts: Vec<String>,
}

impl TopologySnapshot {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Route-only compatibility metadata, not sufficient evidence to arm a runtime.
    pub fn insert(
        &mut self,
        interface: impl Into<String>,
        v4: Option<Ipv4Addr>,
        v6: Option<Ipv6Addr>,
    ) {
        self.insert_routes(interface, v4.map(RouteMode::Via), v6.map(RouteMode::Via));
    }

    pub fn insert_routes(
        &mut self,
        interface: impl Into<String>,
        route_v4: Option<RouteMode<Ipv4Addr>>,
        route_v6: Option<RouteMode<Ipv6Addr>>,
    ) {
        self.interfaces.insert(
            interface.into(),
            InterfaceTopology {
                route_v4,
                route_v6,
                ..Default::default()
            },
        );
    }

    pub fn interface(&self, name: &str) -> Option<&InterfaceTopology> {
        self.interfaces.get(name)
    }
    pub fn is_observed(&self) -> bool {
        self.observed
    }
    pub fn routing_conflicts(&self) -> &[String] {
        &self.conflicts
    }

    pub fn system_default(&self, family: IpFamily) -> Result<Option<&str>, TopologyError> {
        match &self.defaults[family_index(family)] {
            DefaultSelection::None => Ok(None),
            DefaultSelection::Interface(name) => Ok(Some(name)),
            DefaultSelection::Ambiguous(reason) => Err(TopologyError(reason.clone())),
        }
    }
}

fn family_index(family: IpFamily) -> usize {
    match family {
        IpFamily::V4 => 0,
        IpFamily::V6 => 1,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyError(String);
impl fmt::Display for TopologyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for TopologyError {}

pub struct TopologyInput<'a> {
    pub links: &'a [u8],
    pub addresses: &'a [u8],
    pub routes_v4: &'a [u8],
    pub routes_v6: &'a [u8],
    pub rules_v4: &'a [u8],
    pub rules_v6: &'a [u8],
}

#[derive(Deserialize)]
struct Link {
    ifindex: u32,
    ifname: String,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    operstate: String,
    #[serde(default)]
    link_type: String,
    address: Option<String>,
    linkinfo: Option<LinkInfo>,
}

#[derive(Deserialize)]
struct LinkInfo {
    info_kind: Option<String>,
}

#[derive(Deserialize)]
struct AddressList {
    ifindex: u32,
    ifname: String,
    #[serde(default)]
    addr_info: Vec<Address>,
}

#[derive(Deserialize)]
struct Address {
    family: String,
    local: IpAddr,
    scope: Value,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    tentative: bool,
    #[serde(default)]
    dadfailed: bool,
    #[serde(default)]
    deprecated: bool,
    preferred_life_time: Option<Value>,
    valid_life_time: Option<Value>,
}

#[derive(Clone, Deserialize)]
struct Route {
    dst: Option<String>,
    dev: Option<String>,
    gateway: Option<IpAddr>,
    table: Option<Value>,
    #[serde(default)]
    metric: u32,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    nexthops: Vec<Value>,
    scope: Option<Value>,
    #[serde(rename = "type")]
    kind: Option<Value>,
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, TopologyError> {
    if bytes.len() > 1024 * 1024 {
        return Err(TopologyError("topology JSON exceeds size limit".into()));
    }
    serde_json::from_slice(bytes).map_err(|e| TopologyError(format!("invalid topology JSON: {e}")))
}

pub fn parse_topology(input: TopologyInput<'_>) -> Result<TopologySnapshot, TopologyError> {
    let links: Vec<Link> = decode(input.links)?;
    let addresses: Vec<AddressList> = decode(input.addresses)?;
    let mut snapshot = TopologySnapshot {
        observed: true,
        ..Default::default()
    };
    for link in links {
        if link.ifindex == 0
            || link.ifname.is_empty()
            || snapshot.interfaces.contains_key(&link.ifname)
        {
            return Err(TopologyError(
                "invalid or duplicate kernel interface identity".into(),
            ));
        }
        let has = |flag: &str| link.flags.iter().any(|f| f == flag);
        let link_ready = has("UP")
            && !has("NO-CARRIER")
            && !has("NO_CARRIER")
            && link.operstate != "DOWN"
            && (has("LOWER_UP") || (has("POINTOPOINT") && link.operstate == "UNKNOWN"));
        let identity = InterfaceIdentity {
            ifindex: link.ifindex,
            name: link.ifname.clone(),
            kind: link
                .linkinfo
                .and_then(|info| info.info_kind)
                .unwrap_or(link.link_type),
            hardware_address: link.address,
        };
        snapshot.interfaces.insert(
            link.ifname,
            InterfaceTopology {
                identity: Some(identity),
                link_ready,
                ..Default::default()
            },
        );
    }
    for list in addresses {
        let Some(interface) = snapshot.interfaces.get_mut(&list.ifname) else {
            continue;
        };
        if interface.identity.as_ref().map(|id| id.ifindex) != Some(list.ifindex) {
            continue;
        }
        for address in list.addr_info {
            let invalid = address.tentative
                || address.dadfailed
                || address.deprecated
                || address
                    .flags
                    .iter()
                    .any(|f| matches!(f.as_str(), "tentative" | "dadfailed" | "deprecated"))
                || address.preferred_life_time.as_ref().and_then(number) == Some(0)
                || address.valid_life_time.as_ref().and_then(number) == Some(0);
            if invalid
                || !matches!(address.scope.as_str(), Some("global" | "0"))
                    && number(&address.scope) != Some(0)
            {
                continue;
            }
            match address.local {
                IpAddr::V4(ip)
                    if address.family == "inet"
                        && !ip.is_unspecified()
                        && !ip.is_loopback()
                        && !ip.is_multicast()
                        && !ip.is_link_local() =>
                {
                    interface.source_v4.push(ip)
                }
                IpAddr::V6(ip)
                    if address.family == "inet6"
                        && !ip.is_unspecified()
                        && !ip.is_loopback()
                        && !ip.is_multicast()
                        && !ip.is_unicast_link_local() =>
                {
                    interface.source_v6.push(ip)
                }
                _ => {}
            }
        }
        interface.source_v4.sort();
        interface.source_v4.dedup();
        interface.source_v6.sort();
        interface.source_v6.dedup();
    }
    for (family, route_bytes, rule_bytes) in [
        (IpFamily::V4, input.routes_v4, input.rules_v4),
        (IpFamily::V6, input.routes_v6, input.rules_v6),
    ] {
        let routes: Vec<Route> = decode(route_bytes)?;
        let rules: Vec<Value> = decode(rule_bytes)?;
        fill_family(&mut snapshot, family, routes, rules)?;
    }
    Ok(snapshot)
}

pub fn capture_system_topology() -> Result<TopologySnapshot, TopologyError> {
    use TopologyQuery::*;
    let run =
        |q| super::privileged::run_topology_query(q).map_err(|e| TopologyError(e.to_string()));
    let links = run(Links)?;
    let addresses = run(Addresses)?;
    let routes_v4 = run(Routes4)?;
    let routes_v6 = run(Routes6)?;
    let rules_v4 = run(Rules4)?;
    let rules_v6 = run(Rules6)?;
    parse_topology(TopologyInput {
        links: &links,
        addresses: &addresses,
        routes_v4: &routes_v4,
        routes_v6: &routes_v6,
        rules_v4: &rules_v4,
        rules_v6: &rules_v6,
    })
}

fn number(v: &Value) -> Option<u32> {
    v.as_u64()
        .and_then(|n| n.try_into().ok())
        .or_else(|| v.as_str()?.parse().ok())
}

fn table(value: Option<&Value>) -> Result<u32, TopologyError> {
    match value {
        None => Ok(254),
        Some(v) => number(v)
            .or_else(|| match v.as_str()? {
                "main" => Some(254),
                "local" => Some(255),
                "default" => Some(253),
                _ => None,
            })
            .ok_or_else(|| TopologyError("unknown route table representation".into())),
    }
}

fn fill_family(
    s: &mut TopologySnapshot,
    family: IpFamily,
    routes: Vec<Route>,
    rules: Vec<Value>,
) -> Result<(), TopologyError> {
    let mut candidates: BTreeMap<String, Vec<Route>> = BTreeMap::new();
    let mut ambiguity = None;
    for route in routes {
        let route_table = table(route.table.as_ref())?;
        if route_table != 254 {
            continue;
        }
        if route
            .flags
            .iter()
            .any(|flag| flag == "linkdown" || flag == "dead")
        {
            continue;
        }
        let default = matches!(route.dst.as_deref(), Some("default" | "0.0.0.0/0" | "::/0"));
        if let Some(kind) = &route.kind {
            if kind.as_str() != Some("unicast")
                && kind.as_str() != Some("1")
                && number(kind) != Some(1)
            {
                if default {
                    let reason =
                        format!("{family:?}: terminal or unsupported system default route");
                    s.conflicts.push(reason.clone());
                    ambiguity = Some(reason);
                }
                continue;
            }
        }
        if default && !route.nexthops.is_empty() {
            ambiguity = Some(
                "system default is multipath; choose a primary connection explicitly".to_owned(),
            );
            continue;
        }
        let Some(name) = &route.dev else {
            continue;
        };
        let Some(interface) = s.interfaces.get_mut(name) else {
            continue;
        };
        if !default {
            if route.gateway.is_none()
                && route.scope.as_ref().is_some_and(|scope| {
                    scope.as_str() == Some("link")
                        || scope.as_str() == Some("253")
                        || number(scope) == Some(253)
                })
            {
                if let Some(net) = route.dst.as_ref().and_then(|dst| dst.parse::<IpNet>().ok()) {
                    if net.prefix_len() != 0 && net.addr().is_ipv4() == (family == IpFamily::V4) {
                        interface.connected_networks.push(net.trunc());
                    }
                }
            }
            continue;
        }
        if !interface.link_ready {
            continue;
        }
        match (family, route.gateway) {
            (IpFamily::V4, Some(IpAddr::V6(_))) | (IpFamily::V6, Some(IpAddr::V4(_))) => {
                return Err(TopologyError("gateway address family mismatch".into()))
            }
            _ => {}
        }
        candidates.entry(name.clone()).or_default().push(route);
    }
    let mut available: Vec<(u32, String)> = Vec::new();
    for (name, mut routes) in candidates {
        routes.sort_by_key(|r| r.metric);
        let best = &routes[0];
        if routes
            .iter()
            .any(|r| r.metric == best.metric && r.gateway != best.gateway)
        {
            ambiguity = Some(format!("multiple equal default routes for {name}"));
            continue;
        }
        let interface = s
            .interfaces
            .get_mut(&name)
            .expect("known candidate interface");
        match (family, best.gateway) {
            (IpFamily::V4, gateway) if !interface.source_v4.is_empty() => {
                interface.route_v4 = Some(match gateway {
                    Some(IpAddr::V4(ip)) => RouteMode::Via(ip),
                    None => RouteMode::Device,
                    _ => unreachable!(),
                });
            }
            (IpFamily::V6, gateway) if !interface.source_v6.is_empty() => {
                interface.route_v6 = Some(match gateway {
                    Some(IpAddr::V6(ip)) => RouteMode::Via(ip),
                    None => RouteMode::Device,
                    _ => unreachable!(),
                });
            }
            _ => continue,
        }
        available.push((best.metric, name));
    }
    for rule in rules {
        if !ordinary_rule(&rule)? {
            let reason =
                format!("{family:?}: foreign policy routing prevents a unique system default");
            s.conflicts.push(reason.clone());
            ambiguity = Some(reason);
        }
    }
    available.sort();
    let selected = if let Some(reason) = ambiguity {
        DefaultSelection::Ambiguous(reason)
    } else if available.len() > 1 && available[0].0 == available[1].0 {
        DefaultSelection::Ambiguous(
            "equal-metric default exits; choose a primary connection explicitly".into(),
        )
    } else {
        available
            .first()
            .map(|(_, name)| DefaultSelection::Interface(name.clone()))
            .unwrap_or_default()
    };
    s.defaults[family_index(family)] = selected;
    for interface in s.interfaces.values_mut() {
        interface.connected_networks.sort();
        interface.connected_networks.dedup();
    }
    Ok(())
}

fn ordinary_rule(rule: &Value) -> Result<bool, TopologyError> {
    let object = rule
        .as_object()
        .ok_or_else(|| TopologyError("policy rule is not an object".into()))?;
    // Unknown selectors must not turn a foreign rule into an owned/builtin one.
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "priority"
                | "table"
                | "src"
                | "dst"
                | "from"
                | "to"
                | "protocol"
                | "flags"
                | "fwmark"
                | "fwmask"
        )
    }) {
        return Ok(false);
    }
    for key in ["src", "dst", "from", "to"] {
        if object.get(key).is_some_and(|v| v.as_str() != Some("all")) {
            return Ok(false);
        }
    }
    if object
        .get("flags")
        .is_some_and(|flags| !matches!(flags.as_array(), Some(a) if a.is_empty()))
    {
        return Ok(false);
    }
    let priority = object.get("priority").and_then(number);
    let table = table(object.get("table"))?;
    if object.get("fwmark").is_none()
        && object.get("fwmask").is_none()
        && matches!(
            (priority, table),
            (Some(0), 255) | (Some(32766), 254) | (Some(32767), 253)
        )
    {
        return Ok(true);
    }
    let mark = object.get("fwmark").and_then(|v| {
        number(v).or_else(|| u32::from_str_radix(v.as_str()?.strip_prefix("0x")?, 16).ok())
    });
    let mask = object
        .get("fwmask")
        .map(|v| {
            number(v).or_else(|| u32::from_str_radix(v.as_str()?.strip_prefix("0x")?, 16).ok())
        })
        .unwrap_or(Some(u32::MAX));
    if (TABLE_BASE..=TABLE_MAX).contains(&table) && mask == Some(u32::MAX) {
        let slot = table - TABLE_BASE;
        return Ok(mark == Some(MARK_BASE + slot) && priority == Some(RULE_PRIORITY_BASE + slot));
    }
    Ok(false)
}

pub(crate) enum TopologyQuery {
    Links,
    Addresses,
    Routes4,
    Routes6,
    Rules4,
    Rules6,
}
impl TopologyQuery {
    pub(crate) fn arguments(&self) -> &'static [&'static str] {
        match self {
            Self::Links => &["-j", "-d", "-N", "link", "show"],
            Self::Addresses => &["-j", "-N", "address", "show"],
            Self::Routes4 => &["-j", "-4", "-N", "route", "show", "table", "all"],
            Self::Routes6 => &["-j", "-6", "-N", "route", "show", "table", "all"],
            Self::Rules4 => &["-j", "-4", "-N", "rule", "show"],
            Self::Rules6 => &["-j", "-6", "-N", "rule", "show"],
        }
    }
}
