use serde::{Deserialize, Serialize};

use crate::backend::PlanHash;
use crate::domain::{Profile, ProfileId, RuleId};
use crate::health::HealthReport;

pub const IPC_PROTOCOL_VERSION: u32 = 1;

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
    ListProfiles,
    ApplyProfile(Profile),
    SetRuleEnabled {
        profile_id: ProfileId,
        rule_id: RuleId,
        enabled: bool,
    },
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum DaemonResult {
    Status(DaemonStatus),
    Profiles(Vec<Profile>),
    Applied(ApplyProfileResult),
    Deactivated,
    Subscribed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub generation: u64,
    pub health: HealthReport,
    pub desired_profile_id: Option<ProfileId>,
    pub applied_profile_id: Option<ProfileId>,
    pub last_error: Option<IpcError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyProfileResult {
    pub generation: u64,
    pub plan_hash: PlanHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", content = "value")]
pub enum DaemonEvent {
    StateChanged {
        generation: u64,
        health: HealthReport,
    },
    OperationFailed {
        generation: u64,
        health: HealthReport,
        error: IpcError,
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
