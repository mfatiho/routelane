//! Frozen V2 wire DTOs. Do not add new routing semantics to this schema.
use serde::{Deserialize, Deserializer, Serialize};

use super::Language;
use crate::domain::{Lane, Profile, ProfileId, RoutingPolicy, Rule};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigV2 {
    #[serde(deserialize_with = "version_two")]
    pub schema_version: u32,
    #[serde(default)]
    pub language: Language,
    #[serde(default)]
    pub profiles: Vec<ProfileV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileV2 {
    pub id: ProfileId,
    pub name: String,
    pub enabled: bool,
    pub lanes: Vec<Lane>,
    pub rules: Vec<Rule>,
}

impl From<ProfileV2> for Profile {
    fn from(old: ProfileV2) -> Self {
        Self {
            id: old.id,
            name: old.name,
            enabled: false,
            lanes: old.lanes,
            rules: old.rules,
            routing: RoutingPolicy::default(),
        }
    }
}

fn version_two<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let version = u32::deserialize(deserializer)?;
    if version != 2 {
        return Err(serde::de::Error::custom(
            "expected configuration schema version 2",
        ));
    }
    Ok(version)
}
