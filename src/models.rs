use routelane::domain::Profile;
use routelane::ipc::{
    ApplicationAdoptionResult, ApplicationLaunchResult, ApplyProfileResult, DaemonEvent,
    DaemonRequest, DaemonStatus, IpcError,
};

#[derive(Debug)]
pub enum UiToDaemon {
    Request(DaemonRequest),
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum DaemonToUi {
    ConnectionEstablished { epoch: u64 },
    Status(DaemonStatus),
    Profiles(Vec<Profile>),
    Applied(ApplyProfileResult),
    ApplicationLaunched(ApplicationLaunchResult),
    ApplicationProcessesAdopted(ApplicationAdoptionResult),
    Deactivated,
    Event(DaemonEvent),
    DaemonUnavailable(String),
    Error(IpcError),
}
