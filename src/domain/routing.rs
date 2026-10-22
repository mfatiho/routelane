use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{DomainError, Lane, LaneId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PrimarySelection {
    #[default]
    FollowSystemDefault,
    Lane(LaneId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingPolicy {
    pub unmatched_traffic: UnmatchedTraffic,
    pub primary: PrimarySelection,
    pub backups: Vec<LaneId>,
    pub automatic_failover: bool,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self {
            unmatched_traffic: UnmatchedTraffic::PreserveSystem,
            primary: PrimarySelection::FollowSystemDefault,
            backups: Vec::new(),
            automatic_failover: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UnmatchedTraffic {
    #[default]
    PreserveSystem,
    UsePrimary,
}

impl RoutingPolicy {
    pub(crate) fn validate(&self, lanes: &[Lane]) -> Result<(), DomainError> {
        let exists = |id: &LaneId| lanes.iter().any(|lane| &lane.id == id);
        if let PrimarySelection::Lane(id) = &self.primary {
            if !exists(id) {
                return Err(DomainError::UnknownLane(id.clone()));
            }
        }
        let mut seen = BTreeSet::new();
        for backup in &self.backups {
            if !exists(backup) {
                return Err(DomainError::UnknownLane(backup.clone()));
            }
            if self.primary == PrimarySelection::Lane(backup.clone()) {
                return Err(DomainError::PrimaryIsBackup(backup.clone()));
            }
            if !seen.insert(backup) {
                return Err(DomainError::DuplicateBackupLane(backup.clone()));
            }
        }
        Ok(())
    }
}
