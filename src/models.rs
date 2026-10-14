use routelane::domain::Profile;
use routelane::ipc::{DaemonEvent, DaemonRequest, DaemonResponse, DaemonStatus, IpcError};

#[derive(Debug)]
pub enum UiToDaemon {
    Request(UiRequest),
    RefreshSnapshot,
    Shutdown,
}

#[derive(Debug, Clone)]
pub struct UiRequest {
    pub request_id: String,
    pub request: DaemonRequest,
}

impl UiRequest {
    pub fn new(request: DaemonRequest) -> Self {
        Self {
            request_id: uuid::Uuid::new_v4().to_string(),
            request,
        }
    }
}

#[derive(Debug, Clone)]
pub enum DaemonToUi {
    ConnectionEstablished {
        epoch: u64,
    },
    Snapshot {
        epoch: u64,
        status: DaemonStatus,
        profiles: Vec<Profile>,
    },
    Response {
        epoch: u64,
        response: DaemonResponse,
    },
    Event {
        epoch: u64,
        event: DaemonEvent,
    },
    DaemonUnavailable {
        epoch: u64,
        request_id: Option<String>,
        error: IpcError,
    },
}
