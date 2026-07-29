use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::IpcError;

pub const MAX_FRAME_BYTES: usize = 1_048_576;

pub async fn read_frame<R, T>(reader: &mut R) -> Result<T, IpcError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut length_bytes = [0_u8; 4];
    reader
        .read_exact(&mut length_bytes)
        .await
        .map_err(read_error)?;
    let length = u32::from_be_bytes(length_bytes) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(IpcError::new(
            "IPC_FRAME_TOO_LARGE",
            format!("frame is {length} bytes; maximum is {MAX_FRAME_BYTES}"),
        ));
    }

    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await.map_err(read_error)?;
    let json = std::str::from_utf8(&payload).map_err(|error| {
        IpcError::new("IPC_INVALID_UTF8", format!("frame is not UTF-8: {error}"))
    })?;
    serde_json::from_str(json).map_err(|error| {
        IpcError::new(
            "IPC_INVALID_JSON",
            format!("frame JSON is invalid: {error}"),
        )
    })
}

pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), IpcError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = serde_json::to_vec(value).map_err(|error| {
        IpcError::new(
            "IPC_JSON_ENCODE_FAILED",
            format!("could not encode JSON frame: {error}"),
        )
    })?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(IpcError::new(
            "IPC_FRAME_TOO_LARGE",
            format!(
                "frame is {} bytes; maximum is {MAX_FRAME_BYTES}",
                payload.len()
            ),
        ));
    }
    writer
        .write_all(&(payload.len() as u32).to_be_bytes())
        .await
        .map_err(write_error)?;
    writer.write_all(&payload).await.map_err(write_error)?;
    writer.flush().await.map_err(write_error)
}

fn read_error(error: std::io::Error) -> IpcError {
    let (code, message) = if error.kind() == std::io::ErrorKind::UnexpectedEof {
        (
            "IPC_UNEXPECTED_EOF",
            "frame ended before its declared length",
        )
    } else {
        ("IPC_READ_FAILED", "could not read IPC frame")
    };
    IpcError::new(code, format!("{message}: {error}"))
}

fn write_error(error: std::io::Error) -> IpcError {
    IpcError::new(
        "IPC_WRITE_FAILED",
        format!("could not write IPC frame: {error}"),
    )
}
