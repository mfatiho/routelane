use routelane::domain::Profile;
use routelane::ipc::{ApplyProfileResult, DaemonEvent, DaemonRequest, DaemonStatus, IpcError};

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
    Deactivated,
    Event(DaemonEvent),
    DaemonUnavailable(String),
    Error(IpcError),
}
