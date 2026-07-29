use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

use crate::daemon::{
    ConfigRepository, DaemonService, PrivilegedHelperClient, ResolutionProvider, TopologyProvider,
};

use super::{
    read_frame, write_frame, DaemonEnvelope, DaemonEvent, DaemonRequest, DaemonResponse, IpcError,
};

pub fn socket_path() -> Result<PathBuf, IpcError> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
        IpcError::new(
            "IPC_MISSING_RUNTIME_DIRECTORY",
            "XDG_RUNTIME_DIR is not set",
        )
    })?;
    Ok(PathBuf::from(runtime).join("routelane/routelane.sock"))
}

#[async_trait]
pub trait DaemonRequestHandler: Send + Sync {
    async fn handle_request(&self, envelope: DaemonEnvelope) -> DaemonResponse;
    fn subscribe_events(&self) -> broadcast::Receiver<DaemonEvent>;
}

#[async_trait]
impl<T, R, H, C> DaemonRequestHandler for DaemonService<T, R, H, C>
where
    T: TopologyProvider + 'static,
    R: ResolutionProvider + 'static,
    H: PrivilegedHelperClient + 'static,
    C: ConfigRepository + 'static,
{
    async fn handle_request(&self, envelope: DaemonEnvelope) -> DaemonResponse {
        self.handle(envelope).await
    }

    fn subscribe_events(&self) -> broadcast::Receiver<DaemonEvent> {
        self.subscribe()
    }
}

pub struct UnixSocketServer<S> {
    listener: UnixListener,
    service: Arc<S>,
    daemon_uid: u32,
    _instance_lock: File,
}

impl<S> std::fmt::Debug for UnixSocketServer<S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UnixSocketServer")
            .field("daemon_uid", &self.daemon_uid)
            .finish_non_exhaustive()
    }
}

impl<S> UnixSocketServer<S>
where
    S: DaemonRequestHandler + 'static,
{
    pub async fn bind_at(path: PathBuf, service: Arc<S>) -> Result<Self, IpcError> {
        let daemon_uid = effective_uid();
        prepare_parent(&path, daemon_uid)?;
        let instance_lock = acquire_instance_lock(&path, daemon_uid)?;
        remove_verified_stale_socket(&path, daemon_uid)?;
        let listener = UnixListener::bind(&path).map_err(|error| {
            IpcError::new(
                "IPC_SOCKET_BIND_FAILED",
                format!("could not bind {}: {error}", path.display()),
            )
        })?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            IpcError::new(
                "IPC_SOCKET_PERMISSION_FAILED",
                format!("could not secure {}: {error}", path.display()),
            )
        })?;
        Ok(Self {
            listener,
            service,
            daemon_uid,
            _instance_lock: instance_lock,
        })
    }

    pub async fn run(self) -> Result<(), IpcError> {
        loop {
            let (stream, _) = self.listener.accept().await.map_err(|error| {
                IpcError::new(
                    "IPC_ACCEPT_FAILED",
                    format!("could not accept daemon client: {error}"),
                )
            })?;
            if !same_user_peer(&stream, self.daemon_uid)? {
                continue;
            }
            let service = Arc::clone(&self.service);
            tokio::spawn(async move {
                let _ = serve_connection(stream, service).await;
            });
        }
    }
}

async fn serve_connection<S>(mut stream: UnixStream, service: Arc<S>) -> Result<(), IpcError>
where
    S: DaemonRequestHandler + 'static,
{
    let envelope: DaemonEnvelope = match read_frame(&mut stream).await {
        Ok(envelope) => envelope,
        Err(error) => {
            let response = DaemonResponse::failure("", error.clone());
            let _ = write_frame(&mut stream, &response).await;
            return Err(error);
        }
    };
    let subscribing = matches!(envelope.request, DaemonRequest::Subscribe);
    let mut events = subscribing.then(|| service.subscribe_events());
    let response = service.handle_request(envelope).await;
    let subscribed = response.error.is_none() && subscribing;
    write_frame(&mut stream, &response).await?;
    if !subscribed {
        return Ok(());
    }

    let events = events
        .as_mut()
        .expect("subscribe requests create an event receiver");
    loop {
        match events.recv().await {
            Ok(event) => write_frame(&mut stream, &event).await?,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

fn prepare_parent(path: &Path, daemon_uid: u32) -> Result<(), IpcError> {
    let parent = path
        .parent()
        .ok_or_else(|| IpcError::new("IPC_INVALID_SOCKET_PATH", "socket path has no parent"))?;
    fs::create_dir_all(parent).map_err(|error| {
        IpcError::new(
            "IPC_RUNTIME_DIRECTORY_FAILED",
            format!("could not create {}: {error}", parent.display()),
        )
    })?;
    let metadata = fs::symlink_metadata(parent).map_err(|error| {
        IpcError::new(
            "IPC_RUNTIME_DIRECTORY_FAILED",
            format!("could not inspect {}: {error}", parent.display()),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() || metadata.uid() != daemon_uid {
        return Err(IpcError::new(
            "IPC_UNSAFE_RUNTIME_DIRECTORY",
            "socket parent must be a real directory owned by the daemon user",
        ));
    }
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|error| {
        IpcError::new(
            "IPC_RUNTIME_DIRECTORY_FAILED",
            format!("could not secure {}: {error}", parent.display()),
        )
    })
}

fn acquire_instance_lock(path: &Path, daemon_uid: u32) -> Result<File, IpcError> {
    let parent = path
        .parent()
        .ok_or_else(|| IpcError::new("IPC_INVALID_SOCKET_PATH", "socket path has no parent"))?;
    let lock_path = parent.join("routelane.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&lock_path)
        .map_err(|error| {
            let code = if error.raw_os_error() == Some(libc::ELOOP) {
                "IPC_UNSAFE_LOCK_FILE"
            } else {
                "IPC_LOCK_OPEN_FAILED"
            };
            IpcError::new(
                code,
                format!("could not securely open {}: {error}", lock_path.display()),
            )
        })?;
    let metadata = file.metadata().map_err(|error| {
        IpcError::new(
            "IPC_LOCK_INSPECTION_FAILED",
            format!("could not inspect {}: {error}", lock_path.display()),
        )
    })?;
    if !metadata.is_file() || metadata.uid() != daemon_uid {
        return Err(IpcError::new(
            "IPC_UNSAFE_LOCK_FILE",
            "daemon lock must be a regular file owned by the daemon user",
        ));
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| {
            IpcError::new(
                "IPC_LOCK_PERMISSION_FAILED",
                format!("could not secure {}: {error}", lock_path.display()),
            )
        })?;

    // SAFETY: flock only reads the valid file descriptor and the flags are a
    // supported nonblocking exclusive-lock combination.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        let raw_error = error.raw_os_error();
        if raw_error == Some(libc::EAGAIN) || raw_error == Some(libc::EWOULDBLOCK) {
            return Err(already_running_error());
        }
        return Err(IpcError::new(
            "IPC_LOCK_FAILED",
            format!("could not acquire {}: {error}", lock_path.display()),
        ));
    }
    Ok(file)
}

fn remove_verified_stale_socket(path: &Path, daemon_uid: u32) -> Result<(), IpcError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(IpcError::new(
                "IPC_SOCKET_INSPECTION_FAILED",
                format!("could not inspect {}: {error}", path.display()),
            ));
        }
    };
    if !metadata.file_type().is_socket() || metadata.uid() != daemon_uid {
        return Err(IpcError::new(
            "IPC_UNSAFE_SOCKET_PATH",
            "existing socket path is not an owned Unix socket",
        ));
    }
    match StdUnixStream::connect(path) {
        Ok(_) => return Err(already_running_error()),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
        Err(error) => {
            return Err(IpcError::new(
                "IPC_SOCKET_LIVENESS_UNCERTAIN",
                format!(
                    "could not prove that owned socket {} is stale: {error}",
                    path.display()
                ),
            ));
        }
    }
    fs::remove_file(path).map_err(|error| {
        IpcError::new(
            "IPC_STALE_SOCKET_REMOVAL_FAILED",
            format!(
                "could not remove owned stale socket {}: {error}",
                path.display()
            ),
        )
    })
}

fn already_running_error() -> IpcError {
    IpcError::new(
        "IPC_ALREADY_RUNNING",
        "another RouteLane daemon instance is already running",
    )
}

fn same_user_peer(stream: &UnixStream, daemon_uid: u32) -> Result<bool, IpcError> {
    let credentials = stream.peer_cred().map_err(|error| {
        IpcError::new(
            "IPC_PEER_CREDENTIALS_FAILED",
            format!("could not read SO_PEERCRED: {error}"),
        )
    })?;
    Ok(credentials.uid() == daemon_uid)
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    unsafe { libc::geteuid() }
}
