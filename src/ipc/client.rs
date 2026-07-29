use std::path::PathBuf;

use tokio::net::UnixStream;
use tokio::sync::mpsc;

use super::{
    read_frame, write_frame, DaemonEnvelope, DaemonEvent, DaemonRequest, DaemonResponse,
    DaemonResult, IpcError,
};

#[derive(Debug, Clone)]
pub struct DaemonClient {
    path: PathBuf,
}

impl DaemonClient {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub async fn send(&self, envelope: DaemonEnvelope) -> Result<DaemonResponse, IpcError> {
        let mut stream = UnixStream::connect(&self.path)
            .await
            .map_err(connect_error)?;
        write_frame(&mut stream, &envelope).await?;
        read_frame(&mut stream).await
    }

    pub async fn subscribe(
        &self,
        request_id: impl Into<String>,
    ) -> Result<mpsc::Receiver<DaemonEvent>, IpcError> {
        let mut stream = UnixStream::connect(&self.path)
            .await
            .map_err(connect_error)?;
        let envelope = DaemonEnvelope::new(request_id, DaemonRequest::Subscribe);
        write_frame(&mut stream, &envelope).await?;
        let response: DaemonResponse = read_frame(&mut stream).await?;
        if let Some(error) = response.error {
            return Err(error);
        }
        if !matches!(response.result, Some(DaemonResult::Subscribed)) {
            return Err(IpcError::new(
                "IPC_INVALID_RESPONSE",
                "subscribe request did not receive a subscription acknowledgement",
            ));
        }

        let (sender, receiver) = mpsc::channel(32);
        tokio::spawn(async move {
            while let Ok(event) = read_frame(&mut stream).await {
                if sender.send(event).await.is_err() {
                    break;
                }
            }
        });
        Ok(receiver)
    }
}

fn connect_error(error: std::io::Error) -> IpcError {
    IpcError::new(
        "IPC_CONNECT_FAILED",
        format!("could not connect to daemon socket: {error}"),
    )
}
