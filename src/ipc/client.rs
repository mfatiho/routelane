use std::path::PathBuf;

use tokio::net::UnixStream;
use tokio::sync::mpsc;

use super::{
    read_frame, write_frame, DaemonEnvelope, DaemonEvent, DaemonRequest, DaemonResponse,
    DaemonResult, IpcError, IPC_PROTOCOL_VERSION,
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
        let expected_request_id = envelope.request_id.clone();
        let mut stream = UnixStream::connect(&self.path)
            .await
            .map_err(connect_error)?;
        write_frame(&mut stream, &envelope).await?;
        let response = read_frame(&mut stream).await?;
        validate_response_identity(&response, &expected_request_id)?;
        Ok(response)
    }

    pub async fn subscribe(
        &self,
        request_id: impl Into<String>,
    ) -> Result<mpsc::Receiver<DaemonEvent>, IpcError> {
        let mut stream = UnixStream::connect(&self.path)
            .await
            .map_err(connect_error)?;
        let envelope = DaemonEnvelope::new(request_id, DaemonRequest::Subscribe);
        let expected_request_id = envelope.request_id.clone();
        write_frame(&mut stream, &envelope).await?;
        let response: DaemonResponse = read_frame(&mut stream).await?;
        validate_response_identity(&response, &expected_request_id)?;
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

fn validate_response_identity(
    response: &DaemonResponse,
    expected_request_id: &str,
) -> Result<(), IpcError> {
    if response.protocol_version != IPC_PROTOCOL_VERSION {
        return Err(IpcError::new(
            "IPC_UNSUPPORTED_PROTOCOL_VERSION",
            format!(
                "response protocol version {} is unsupported; expected {IPC_PROTOCOL_VERSION}",
                response.protocol_version
            ),
        ));
    }
    if response.request_id != expected_request_id {
        return Err(IpcError::new(
            "IPC_RESPONSE_ID_MISMATCH",
            "daemon response request_id did not match the request",
        ));
    }
    Ok(())
}

fn connect_error(error: std::io::Error) -> IpcError {
    IpcError::new(
        "IPC_CONNECT_FAILED",
        format!("could not connect to daemon socket: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::validate_response_identity;
    use crate::ipc::{DaemonResponse, DaemonResult};

    #[test]
    fn rejects_mismatched_response_version_and_request_id() {
        let mut response = DaemonResponse::success("wrong", DaemonResult::Deactivated);
        assert_eq!(
            validate_response_identity(&response, "expected")
                .unwrap_err()
                .code,
            "IPC_RESPONSE_ID_MISMATCH"
        );

        response.request_id = "expected".to_owned();
        response.protocol_version += 1;
        assert_eq!(
            validate_response_identity(&response, "expected")
                .unwrap_err()
                .code,
            "IPC_UNSUPPORTED_PROTOCOL_VERSION"
        );
    }
}
