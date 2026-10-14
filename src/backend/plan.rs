use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr};

use ipnet::IpNet;
use serde::{Deserialize, Serialize};

use super::{InterfaceIdentity, IpFamily, PrimaryDecision};
use crate::domain::{LaneFailurePolicy, LaneId, ProfileId, RuleId};

pub const TABLE_BASE: u32 = 20_000;
pub const TABLE_MAX: u32 = 20_099;
pub const MARK_BASE: u32 = 0x524c0001;
pub const MARK_MAX: u32 = 0x524c0064;
pub const RULE_PRIORITY_BASE: u32 = 10_000;
pub const RULE_PRIORITY_MAX: u32 = 10_099;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelPlan {
    pub primary: PrimaryDecision,
    pub local_networks: Vec<IpNet>,
    pub generation: u64,
    pub profile_id: ProfileId,
    pub lanes: Vec<LanePlan>,
    pub rules: Vec<CompiledRule>,
}

impl KernelPlan {
    pub fn content_hash(&self) -> PlanHash {
        #[derive(Serialize)]
        struct PolicyContent<'a> {
            primary: &'a PrimaryDecision,
            local_networks: &'a [IpNet],
            profile_id: &'a ProfileId,
            lanes: &'a [LanePlan],
            rules: &'a [CompiledRule],
        }

        let content = PolicyContent {
            primary: &self.primary,
            local_networks: &self.local_networks,
            profile_id: &self.profile_id,
            lanes: &self.lanes,
            rules: &self.rules,
        };
        let canonical = serde_json::to_vec(&content)
            .expect("serializing kernel plan policy content cannot fail");

        PlanHash(*blake3::hash(&canonical).as_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanePlan {
    pub identity: Option<InterfaceIdentity>,
    pub source_v4: Vec<Ipv4Addr>,
    pub source_v6: Vec<Ipv6Addr>,
    pub id: LaneId,
    pub interface: String,
    pub route_v4: Option<RouteMode<Ipv4Addr>>,
    pub route_v6: Option<RouteMode<Ipv6Addr>>,
    pub availability: LaneAvailability,
    pub table: u32,
    pub mark: u32,
    pub rule_priority: u32,
    pub failure_policy: LaneFailurePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteMode<A> {
    Via(A),
    Device,
}

impl<A> RouteMode<A> {
    pub fn gateway(&self) -> Option<&A> {
        match self {
            Self::Via(gateway) => Some(gateway),
            Self::Device => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaneAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledRule {
    pub family: IpFamily,
    pub id: RuleId,
    pub priority: u32,
    pub target: CompiledTarget,
    pub action: CompiledAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompiledTarget {
    Any,
    Cgroup(String),
    Network(IpNet),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompiledAction {
    Mark(u32),
    Direct,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PlanHash([u8; 32]);

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResolutionSnapshot {
    rules: BTreeMap<RuleId, Vec<IpNet>>,
}

impl ResolutionSnapshot {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_rule(rule_id: RuleId, addresses: Vec<IpNet>) -> Self {
        Self {
            rules: BTreeMap::from([(rule_id, addresses)]),
        }
    }

    pub fn insert(&mut self, rule_id: RuleId, addresses: Vec<IpNet>) {
        self.rules.insert(rule_id, addresses);
    }

    pub(crate) fn addresses(&self, rule_id: &RuleId) -> Option<&[IpNet]> {
        self.rules.get(rule_id).map(Vec::as_slice)
    }
}
