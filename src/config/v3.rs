//! Frozen schema-3 wire representation, before selective routing.
//! Never use the current RoutingPolicy here: it would change old policy hashes.
use serde::{Deserialize, Deserializer, Serialize};

use super::Language;
use crate::domain::{
    Lane, LaneId, PrimarySelection, Profile, ProfileId, RoutingPolicy, Rule, UnmatchedTraffic,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigV3 {
    #[serde(deserialize_with = "version_three")]
    pub schema_version: u32,
    #[serde(default)]
    pub routing_notice_pending: bool,
    #[serde(default)]
    pub language: Language,
    #[serde(default)]
    pub profiles: Vec<ProfileV3>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileV3 {
    pub id: ProfileId,
    pub name: String,
    pub enabled: bool,
    pub lanes: Vec<Lane>,
    pub rules: Vec<Rule>,
    pub routing: RoutingPolicyV3,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingPolicyV3 {
    pub primary: PrimarySelection,
    pub backups: Vec<LaneId>,
    pub automatic_failover: bool,
}

impl From<ProfileV3> for Profile {
    fn from(old: ProfileV3) -> Self {
        Self {
            id: old.id,
            name: old.name,
            enabled: false,
            lanes: old.lanes,
            rules: old.rules,
            routing: RoutingPolicy {
                unmatched_traffic: UnmatchedTraffic::PreserveSystem,
                primary: old.routing.primary,
                backups: old.routing.backups,
                automatic_failover: old.routing.automatic_failover,
            },
        }
    }
}

fn version_three<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let version = u32::deserialize(deserializer)?;
    if version != 3 {
        return Err(serde::de::Error::custom(
            "expected configuration schema version 3",
        ));
    }
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_config_v3_rejects_a_future_schema() {
        let config: ConfigV3 = serde_json::from_str(r#"{"schema_version":3}"#).unwrap();
        assert!(config.profiles.is_empty());
        assert!(serde_json::from_str::<ConfigV3>(r#"{"schema_version":4}"#).is_err());
    }
}
