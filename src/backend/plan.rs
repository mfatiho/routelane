use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr};

use ipnet::IpNet;
use serde::{Deserialize, Serialize};

use crate::domain::{LaneFailurePolicy, LaneId, ProfileId, RuleId};

pub const TABLE_BASE: u32 = 20_000;
pub const TABLE_MAX: u32 = 20_099;
pub const MARK_BASE: u32 = 0x524c0001;
pub const MARK_MAX: u32 = 0x524c0064;
pub const RULE_PRIORITY_BASE: u32 = 10_000;
pub const RULE_PRIORITY_MAX: u32 = 10_099;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelPlan {
    pub generation: u64,
    pub profile_id: ProfileId,
    pub lanes: Vec<LanePlan>,
    pub rules: Vec<CompiledRule>,
}

impl KernelPlan {
    pub fn content_hash(&self) -> PlanHash {
        #[derive(Serialize)]
        struct PolicyContent<'a> {
            profile_id: &'a ProfileId,
            lanes: &'a [LanePlan],
            rules: &'a [CompiledRule],
        }

        let content = PolicyContent {
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
    pub id: LaneId,
    pub interface: String,
    pub gateway_v4: Option<Ipv4Addr>,
    pub gateway_v6: Option<Ipv6Addr>,
    pub availability: LaneAvailability,
    pub table: u32,
    pub mark: u32,
    pub rule_priority: u32,
    pub failure_policy: LaneFailurePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaneAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledRule {
    pub id: RuleId,
    pub priority: u32,
    pub target: CompiledTarget,
    pub action: CompiledAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompiledTarget {
    Any,
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
pub struct TopologySnapshot {
    interfaces: BTreeMap<String, InterfaceTopology>,
}

impl TopologySnapshot {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn insert(
        &mut self,
        interface: impl Into<String>,
        gateway_v4: Option<Ipv4Addr>,
        gateway_v6: Option<Ipv6Addr>,
    ) {
        self.interfaces.insert(
            interface.into(),
            InterfaceTopology {
                gateway_v4,
                gateway_v6,
            },
        );
    }

    pub(crate) fn interface(&self, interface: &str) -> Option<&InterfaceTopology> {
        self.interfaces.get(interface)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InterfaceTopology {
    pub gateway_v4: Option<Ipv4Addr>,
    pub gateway_v6: Option<Ipv6Addr>,
}

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
