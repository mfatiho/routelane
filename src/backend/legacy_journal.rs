//! Frozen schema-1 journal (`aa29c4d`, before primary routing) for cleanup only.
//!
//! These types must never change: the ownership marker name and the stored
//! nftables fingerprint were derived from their exact serialization. A legacy
//! record can prove and remove what that older helper owned, but it is never
//! converted into a current plan and can never arm routing.

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, Ipv6Addr};

use ipnet::IpNet;
use serde::{Deserialize, Serialize};

use super::RouteMode;
use crate::domain::{LaneFailurePolicy, LaneId, ProfileId, RuleId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyJournalV1 {
    pub active: Option<LegacyPlanV1>,
    pub active_nft_fingerprint: Option<String>,
    pub pending: Option<LegacyPlanV1>,
    #[serde(default)]
    pub pending_expectations: Option<serde_json::Value>,
    #[serde(default)]
    pub cleanup_pending: bool,
    #[serde(default)]
    pub restore_receipt: Option<serde_json::Value>,
    #[serde(default)]
    pub orphan_cleanup: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyPlanV1 {
    pub generation: u64,
    pub profile_id: ProfileId,
    pub lanes: Vec<LegacyLaneV1>,
    pub rules: Vec<LegacyRuleV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyLaneV1 {
    pub id: LaneId,
    pub interface: String,
    pub route_v4: Option<RouteMode<Ipv4Addr>>,
    pub route_v6: Option<RouteMode<Ipv6Addr>>,
    pub availability: LegacyAvailabilityV1,
    pub table: u32,
    pub mark: u32,
    pub rule_priority: u32,
    pub failure_policy: LaneFailurePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegacyAvailabilityV1 {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyRuleV1 {
    pub id: RuleId,
    pub priority: u32,
    pub target: LegacyTargetV1,
    pub action: LegacyActionV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegacyTargetV1 {
    Any,
    Cgroup(String),
    Network(IpNet),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegacyActionV1 {
    Mark(u32),
    Direct,
    Block,
}

/// A unicast default route the schema-1 helper owned.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LegacyRoute {
    pub family: u8,
    pub table: u32,
    pub gateway: Option<String>,
    pub interface: String,
}

/// A policy rule the schema-1 helper owned.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LegacyRule {
    pub family: u8,
    pub priority: u32,
    pub table: u32,
    pub mark: u32,
}

impl LegacyPlanV1 {
    /// The schema-1 policy hash, byte-for-byte as that helper computed it.
    fn content_hash(&self) -> [u8; 32] {
        #[derive(Serialize)]
        struct PolicyContent<'a> {
            profile_id: &'a ProfileId,
            lanes: &'a [LegacyLaneV1],
            rules: &'a [LegacyRuleV1],
        }
        let canonical = serde_json::to_vec(&PolicyContent {
            profile_id: &self.profile_id,
            lanes: &self.lanes,
            rules: &self.rules,
        })
        .expect("serializing a legacy plan cannot fail");
        *blake3::hash(&canonical).as_bytes()
    }

    /// The schema-1 nftables ownership marker chain.
    pub fn state_chain_name(&self) -> String {
        let serialized =
            serde_json::to_vec(&self.content_hash()).expect("serializing a hash cannot fail");
        format!(
            "state_g{}_{:.16}",
            self.generation,
            blake3::hash(&serialized).to_hex()
        )
    }

    /// Exactly the routes and rules the schema-1 helper installed for this plan.
    pub fn ownership(&self) -> (BTreeSet<LegacyRoute>, BTreeSet<LegacyRule>) {
        let mut routes = BTreeSet::new();
        let mut rules = BTreeSet::new();
        for lane in self
            .lanes
            .iter()
            .filter(|lane| lane.availability == LegacyAvailabilityV1::Available)
        {
            let families = [
                (
                    4,
                    lane.route_v4
                        .map(|route| route.gateway().map(ToString::to_string)),
                ),
                (
                    6,
                    lane.route_v6
                        .map(|route| route.gateway().map(ToString::to_string)),
                ),
            ];
            for (family, route) in families {
                let Some(gateway) = route else {
                    continue;
                };
                routes.insert(LegacyRoute {
                    family,
                    table: lane.table,
                    gateway,
                    interface: lane.interface.clone(),
                });
                rules.insert(LegacyRule {
                    family,
                    priority: lane.rule_priority,
                    table: lane.table,
                    mark: lane.mark,
                });
            }
        }
        (routes, rules)
    }
}

/// Schema-1 nftables fingerprint normalization, frozen with the journal.
pub fn legacy_nft_fingerprint(document: &serde_json::Value) -> String {
    fn normalize(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(object) => {
                object.remove("metainfo");
                object.remove("handle");
                if object.get("counter").is_some_and(|counter| {
                    counter.as_object().is_some_and(|counter| {
                        counter.contains_key("packets") || counter.contains_key("bytes")
                    })
                }) {
                    object.insert("counter".to_owned(), serde_json::Value::Null);
                }
                for nested in object.values_mut() {
                    normalize(nested);
                }
            }
            serde_json::Value::Array(values) => {
                for nested in values {
                    normalize(nested);
                }
            }
            _ => {}
        }
    }
    let mut document = document.clone();
    normalize(&mut document);
    let canonical = serde_json::to_vec(&document).expect("encoding nftables JSON cannot fail");
    blake3::hash(&canonical).to_hex().to_string()
}
