use serde::{Deserialize, Serialize};

use crate::backend::{PlanHash, RuntimeStatus};
use crate::domain::{Profile, ProfileId, RuleId};
use crate::health::HealthReport;

/// 2: runtime authority (helper-owned primary/backup decisions).
/// 3: selective routing; status reports the helper's applied routing mode.
pub const IPC_PROTOCOL_VERSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonEnvelope {
    pub protocol_version: u32,
    pub request_id: String,
    pub request: DaemonRequest,
}

impl DaemonEnvelope {
    pub fn new(request_id: impl Into<String>, request: DaemonRequest) -> Self {
        Self {
            protocol_version: IPC_PROTOCOL_VERSION,
            request_id: request_id.into(),
            request,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "parameters")]
pub enum DaemonRequest {
    GetStatus,
    /// Returns the daemon's partial desired/applied profile snapshot. Clients
    /// merge these profiles by ID; absence never authorizes local deletion.
    ListProfiles,
    ApplyProfile(Profile),
    SetRuleEnabled {
        profile_id: ProfileId,
        rule_id: RuleId,
        enabled: bool,
    },
    LaunchApplication {
        profile_id: ProfileId,
        rule_id: RuleId,
    },
    AdoptApplicationProcesses {
        profile_id: ProfileId,
        rule_id: RuleId,
    },
    RecoverOrphanedResources,
    Deactivate,
    Subscribe,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub protocol_version: u32,
    pub request_id: String,
    pub result: Option<DaemonResult>,
    pub error: Option<IpcError>,
}

impl DaemonResponse {
    pub fn success(request_id: impl Into<String>, result: DaemonResult) -> Self {
        Self {
            protocol_version: IPC_PROTOCOL_VERSION,
            request_id: request_id.into(),
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(request_id: impl Into<String>, error: IpcError) -> Self {
        Self {
            protocol_version: IPC_PROTOCOL_VERSION,
            request_id: request_id.into(),
            result: None,
            error: Some(error),
        }
    }
}

// One short-lived message per request; boxing the status would only churn
// every consumer of the wire type.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum DaemonResult {
    Status(DaemonStatus),
    /// Partial profile snapshot for merge/upsert, not an authoritative catalog.
    Profiles(Vec<Profile>),
    Applied(ApplyProfileResult),
    ApplicationLaunched(ApplicationLaunchResult),
    ApplicationProcessesAdopted(ApplicationAdoptionResult),
    OrphanedResourcesRecovered(OrphanRecoveryResult),
    Deactivated,
    Subscribed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub generation: u64,
    pub event_sequence: u64,
    pub health: HealthReport,
    pub desired_profile_id: Option<ProfileId>,
    pub applied_profile_id: Option<ProfileId>,
    pub last_error: Option<IpcError>,
    /// Verified runtime authority from the helper; never derived from config.
    #[serde(default)]
    pub primary: RuntimeStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyProfileResult {
    pub generation: u64,
    pub plan_hash: PlanHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationLaunchResult {
    pub pid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationAdoptionResult {
    pub adopted_count: u32,
    pub skipped_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanRecoveryResult {
    pub cleaned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", content = "value")]
pub enum DaemonEvent {
    StateChanged {
        sequence: u64,
        generation: u64,
        health: HealthReport,
    },
    OperationFailed {
        sequence: u64,
        generation: u64,
        health: HealthReport,
        error: IpcError,
    },
    /// The helper changed the effective exit on its own (failover/failback).
    PrimaryChanged {
        sequence: u64,
        generation: u64,
        primary: RuntimeStatus,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpcError {
    pub code: String,
    pub message: String,
}

impl IpcError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for IpcError {}

#[cfg(test)]
mod tests {
    use super::{DaemonEnvelope, DaemonRequest, DaemonResult, OrphanRecoveryResult};

    #[test]
    fn orphan_recovery_request_and_result_have_stable_wire_names() {
        let request = DaemonEnvelope::new("recover-1", DaemonRequest::RecoverOrphanedResources);
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["request"]["method"], "RecoverOrphanedResources");

        let result =
            DaemonResult::OrphanedResourcesRecovered(OrphanRecoveryResult { cleaned: true });
        let encoded = serde_json::to_value(result).unwrap();
        assert_eq!(encoded["kind"], "OrphanedResourcesRecovered");
        assert_eq!(encoded["value"]["cleaned"], true);
    }
}
