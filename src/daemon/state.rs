use crate::backend::{AppliedKernelState, RuntimeStatus};
use crate::domain::Profile;
use crate::health::HealthReport;
use crate::ipc::{DaemonStatus, IpcError};

#[derive(Debug, Clone)]
pub struct DaemonState {
    pub desired: Option<Profile>,
    pub applied: Option<AppliedKernelState>,
    pub applied_profile: Option<Profile>,
    pub generation: u64,
    pub event_sequence: u64,
    pub health: HealthReport,
    pub last_error: Option<IpcError>,
    pub primary: RuntimeStatus,
}

impl DaemonState {
    pub fn new(desired: Option<Profile>) -> Self {
        Self {
            desired,
            applied: None,
            applied_profile: None,
            generation: 0,
            event_sequence: 0,
            health: HealthReport::inactive(),
            last_error: None,
            primary: RuntimeStatus::default(),
        }
    }

    pub fn status(&self) -> DaemonStatus {
        DaemonStatus {
            generation: self.generation,
            event_sequence: self.event_sequence,
            health: self.health.clone(),
            desired_profile_id: self.desired.as_ref().map(|profile| profile.id.clone()),
            applied_profile_id: self
                .applied
                .as_ref()
                .map(|applied| applied.plan().profile_id.clone()),
            last_error: self.last_error.clone(),
            primary: self.primary.clone(),
        }
    }
}
