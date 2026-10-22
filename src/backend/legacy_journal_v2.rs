//! Frozen journal-version 2: primary routing, before selective scope.
//! These types describe ownership evidence, never a modern activatable plan.
use std::collections::BTreeSet;
use std::net::IpAddr;

use ipnet::IpNet;
use serde::{Deserialize, Serialize};

use super::legacy_journal::LegacyJournalV1;
use super::{
    CompiledRule, InterfaceSettingReceipt, LanePlan, PlanHash, PrimaryDecision, PrivilegedError,
    ResolutionSnapshot, RuntimeGrant,
};
use crate::config::v3::ProfileV3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyJournalV2 {
    journal_version: u32,
    pub active: Option<LegacyPlanV2>,
    pub active_nft_fingerprint: Option<String>,
    pub pending: Option<LegacyPlanV2>,
    #[serde(default)]
    pub pending_expectations: Option<PendingExpectationsV2>,
    #[serde(default)]
    pub cleanup_pending: bool,
    #[serde(default)]
    pub restore_receipt: Option<RestoreReceiptV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orphan_cleanup: Option<OrphanedOwnershipV2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interface_settings: Vec<InterfaceSettingReceipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_interface_settings: Vec<InterfaceSettingReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_v1: Option<LegacyJournalV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeRecordV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_runtime: Option<RuntimeRecordV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_restore: Option<RuntimeRestoreReceiptV2>,
}

impl LegacyJournalV2 {
    pub fn decode(json: &[u8]) -> Result<Self, PrivilegedError> {
        let record: Self = serde_json::from_slice(json)
            .map_err(|error| PrivilegedError::new(format!("decode legacy v2 journal: {error}")))?;
        if record.journal_version != 2 {
            return Err(PrivilegedError::new(
                "expected applied-state journal version 2",
            ));
        }
        Ok(record)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyPlanV2 {
    pub primary: PrimaryDecision,
    pub local_networks: Vec<IpNet>,
    pub local_addresses: Vec<IpAddr>,
    pub generation: u64,
    pub profile_id: crate::domain::ProfileId,
    pub lanes: Vec<LanePlan>,
    pub rules: Vec<CompiledRule>,
}

impl LegacyPlanV2 {
    pub fn content_hash(&self) -> PlanHash {
        #[derive(Serialize)]
        struct PolicyContent<'a> {
            primary: &'a PrimaryDecision,
            local_networks: &'a [IpNet],
            local_addresses: &'a [IpAddr],
            profile_id: &'a crate::domain::ProfileId,
            lanes: &'a [LanePlan],
            rules: &'a [CompiledRule],
        }
        let canonical = serde_json::to_vec(&PolicyContent {
            primary: &self.primary,
            local_networks: &self.local_networks,
            local_addresses: &self.local_addresses,
            profile_id: &self.profile_id,
            lanes: &self.lanes,
            rules: &self.rules,
        })
        .expect("legacy policy encoding");
        PlanHash::of_bytes(&canonical)
    }

    pub fn state_chain_name(&self) -> String {
        let encoded = serde_json::to_vec(&self.content_hash()).expect("legacy hash encoding");
        format!(
            "state_g{}_{:.16}",
            self.generation,
            blake3::hash(&encoded).to_hex()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRecordV2 {
    pub grant: RuntimeGrant,
    pub profile: ProfileV3,
    pub resolutions: ResolutionSnapshot,
    pub decision: PrimaryDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRestoreReceiptV2 {
    pub token_hash: String,
    pub owner_uid: u32,
    pub issued_for_generation: u64,
    pub issued_for_plan_hash: PlanHash,
    pub previous_plan: Option<LegacyPlanV2>,
    pub previous_record: Option<RuntimeRecordV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreReceiptV2 {
    pub failed_generation: u64,
    pub failed_plan_hash: PlanHash,
    pub previous_generation: Option<u64>,
    pub previous_plan_hash: Option<PlanHash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingExpectationsV2 {
    pub guard_nft_fingerprint: String,
    pub guardless_transaction_nft_fingerprint: String,
    pub activated_nft_fingerprint: String,
    pub committed_nft_fingerprint: String,
    pub committed_routing_fingerprint: String,
    pub routing_model_fingerprint: String,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub lost_unicast: BTreeSet<(u8, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrphanedOwnershipV2 {
    pub nft_fingerprint: Option<String>,
    pub routing: RoutingOwnershipV2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingOwnershipV2 {
    pub routes: BTreeSet<RouteOwnershipV2>,
    pub rules: BTreeSet<RuleOwnershipV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteOwnershipV2 {
    pub family: u8,
    pub table: u32,
    pub gateway: Option<String>,
    pub interface: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleOwnershipV2 {
    pub family: u8,
    pub priority: u32,
    pub table: u32,
    pub mark: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const COMMITTED: &[u8] = include_bytes!("../../tests/fixtures/legacy-v2/committed.json");
    const PENDING: &[u8] = include_bytes!("../../tests/fixtures/legacy-v2/pending.json");
    const CLEANUP: &[u8] = include_bytes!("../../tests/fixtures/legacy-v2/cleanup-pending.json");

    #[test]
    fn legacy_v2_hash_and_marker_match_frozen_evidence() {
        let evidence: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/legacy-v2/evidence.json"))
                .unwrap();
        let journal = LegacyJournalV2::decode(COMMITTED).unwrap();
        let plan = journal.active.unwrap();
        assert_eq!(
            serde_json::to_value(plan.content_hash()).unwrap(),
            evidence["plan_hash"]
        );
        assert_eq!(plan.state_chain_name(), "state_g1_fd28e61b0433fb55");
        assert_eq!(
            journal.active_nft_fingerprint.as_deref(),
            evidence["nft_fingerprint"].as_str()
        );
        let mut next_generation = plan.clone();
        next_generation.generation += 1;
        assert_eq!(plan.content_hash(), next_generation.content_hash());
    }

    #[test]
    fn legacy_v2_unknown_fields_and_wrong_version_are_rejected() {
        for field in ["journal_version", "future_field"] {
            let mut value: Value = serde_json::from_slice(COMMITTED).unwrap();
            value[field] = json!(3);
            assert!(LegacyJournalV2::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        let mut value: Value = serde_json::from_slice(COMMITTED).unwrap();
        value["runtime"]["profile"]["routing"]["unmatched_traffic"] = json!("PreserveSystem");
        assert!(LegacyJournalV2::decode(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn legacy_profile_does_not_gain_new_policy_fields() {
        let journal = LegacyJournalV2::decode(COMMITTED).unwrap();
        let runtime = journal.runtime.unwrap();
        let encoded = serde_json::to_vec(&(&runtime.profile, &runtime.resolutions)).unwrap();
        assert_eq!(PlanHash::of_bytes(&encoded), runtime.grant.policy_hash);
        assert!(serde_json::to_value(runtime.profile.routing)
            .unwrap()
            .get("unmatched_traffic")
            .is_none());
    }

    #[test]
    fn legacy_v2_pending_and_cleanup_round_trip_without_new_fields() {
        for bytes in [COMMITTED, PENDING, CLEANUP] {
            let record = LegacyJournalV2::decode(bytes).unwrap();
            let original: Value = serde_json::from_slice(bytes).unwrap();
            assert_eq!(serde_json::to_value(record).unwrap(), original);
        }
    }
}
