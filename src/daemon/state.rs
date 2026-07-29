use crate::backend::AppliedKernelState;
use crate::domain::{HealthState, Profile};
use crate::ipc::{DaemonStatus, IpcError};

#[derive(Debug, Clone)]
pub struct DaemonState {
    pub desired: Option<Profile>,
    pub applied: Option<AppliedKernelState>,
    pub generation: u64,
    pub health: HealthState,
    pub last_error: Option<IpcError>,
}

impl DaemonState {
    pub fn new(desired: Option<Profile>) -> Self {
        Self {
            desired,
            applied: None,
            generation: 0,
            health: HealthState::Inactive,
            last_error: None,
        }
    }

    pub fn status(&self) -> DaemonStatus {
        DaemonStatus {
            generation: self.generation,
            health: self.health,
            desired_profile_id: self.desired.as_ref().map(|profile| profile.id.clone()),
            applied_profile_id: self
                .applied
                .as_ref()
                .map(|applied| applied.plan().profile_id.clone()),
            last_error: self.last_error.clone(),
        }
    }
}
