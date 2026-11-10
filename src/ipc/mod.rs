mod client;
mod framing;
mod protocol;
mod server;

pub use client::DaemonClient;
pub use framing::{read_frame, write_frame, MAX_FRAME_BYTES};
pub use protocol::{
    ApplicationAdoptionResult, ApplicationLaunchResult, ApplyProfileResult, DaemonEnvelope,
    DaemonEvent, DaemonRequest, DaemonResponse, DaemonResult, DaemonStatus, IpcError,
    OrphanRecoveryResult, IPC_PROTOCOL_VERSION,
};
pub use server::{socket_path, DaemonRequestHandler, UnixSocketServer};

pub use crate::usage::{
    events::{EventCategory, UsageEvent},
    quota::LaneQuota,
    store::QuotaCrossing,
    LaneUsage, QuotaStatus, UsageRate, UsageReport,
};
