//! Frame encoding and decoding for the OpenUDS protocol.
//!
//! Frame format:
//! ┌───────────────┬──────────────────────────────┐
//! │  payload_len  │        payload (JSON)          │
//! │   (4 bytes)   │     (payload_len bytes)        │
//! └───────────────┴──────────────────────────────┘
//!
//! - payload_len: u32 in little-endian byte order
//! - payload: UTF-8 JSON byte sequence
//! - Max payload: 16 MiB

use serde_json::Value;
use std::io::{Read, Write};

/// Maximum payload size: 16 MiB.
pub const MAX_PAYLOAD_SIZE: u32 = 16 * 1024 * 1024;

/// Error type for protocol operations.
#[derive(Debug)]
pub enum ProtocolError {
    /// Payload exceeds maximum allowed size.
    PayloadTooLarge(u32),
    /// Invalid length prefix (e.g., all zeros for a non-frame).
    InvalidLengthPrefix,
    /// Payload is not valid UTF-8.
    InvalidUtf8(std::str::Utf8Error),
    /// Payload is not valid JSON.
    InvalidJson(serde_json::Error),
    /// I/O error.
    Io(std::io::Error),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PayloadTooLarge(size) => {
                write!(f, "payload too large: {size} > max {MAX_PAYLOAD_SIZE}")
            }
            Self::InvalidLengthPrefix => write!(f, "invalid length prefix"),
            Self::InvalidUtf8(e) => write!(f, "invalid UTF-8: {e}"),
            Self::InvalidJson(e) => write!(f, "invalid JSON: {e}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

impl From<std::io::Error> for ProtocolError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for ProtocolError {
    fn from(e: serde_json::Error) -> Self {
        Self::InvalidJson(e)
    }
}

/// Encode a JSON value into a length-prefixed frame buffer.
pub fn encode(value: &Value) -> Result<Vec<u8>, ProtocolError> {
    let payload = serde_json::to_vec(value)?;
    let len = payload.len() as u32;

    if len > MAX_PAYLOAD_SIZE {
        return Err(ProtocolError::PayloadTooLarge(len));
    }

    let mut buf = Vec::with_capacity(4 + len as usize);
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(&payload);
    Ok(buf)
}

/// Encode a raw UTF-8 string as a length-prefixed frame.
pub fn encode_raw(payload: &str) -> Result<Vec<u8>, ProtocolError> {
    let bytes = payload.as_bytes();
    let len = bytes.len() as u32;

    if len > MAX_PAYLOAD_SIZE {
        return Err(ProtocolError::PayloadTooLarge(len));
    }

    let mut buf = Vec::with_capacity(4 + len as usize);
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(bytes);
    Ok(buf)
}

/// Read one complete frame from a reader, returning the decoded JSON Value.
///
/// This is a blocking read suitable for use in a spawned blocking thread
/// or with `tokio::task::spawn_blocking`. For async reads from a
/// `tokio::net::UnixStream`, use `read_frame_async` instead.
pub fn read_frame(reader: &mut dyn Read) -> Result<Value, ProtocolError> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let payload_len = u32::from_le_bytes(len_buf);

    if payload_len == 0 {
        return Err(ProtocolError::InvalidLengthPrefix);
    }
    if payload_len > MAX_PAYLOAD_SIZE {
        return Err(ProtocolError::PayloadTooLarge(payload_len));
    }

    let mut payload = vec![0u8; payload_len as usize];
    reader.read_exact(&mut payload)?;

    let json_str =
        std::str::from_utf8(&payload).map_err(ProtocolError::InvalidUtf8)?;
    let value: Value = serde_json::from_str(json_str)?;
    Ok(value)
}

/// Async read one complete frame from a tokio UnixStream.
pub async fn read_frame_async(
    stream: &mut tokio::net::UnixStream,
) -> Result<Value, ProtocolError> {
    use tokio::io::AsyncReadExt;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let payload_len = u32::from_le_bytes(len_buf);

    if payload_len == 0 {
        return Err(ProtocolError::InvalidLengthPrefix);
    }
    if payload_len > MAX_PAYLOAD_SIZE {
        return Err(ProtocolError::PayloadTooLarge(payload_len));
    }

    let mut payload = vec![0u8; payload_len as usize];
    stream.read_exact(&mut payload).await?;

    let json_str =
        std::str::from_utf8(&payload).map_err(ProtocolError::InvalidUtf8)?;
    let value: Value = serde_json::from_str(json_str)?;
    Ok(value)
}

/// Async write a JSON value as a length-prefixed frame to a tokio UnixStream.
pub async fn write_frame_async(
    stream: &mut tokio::net::UnixStream,
    value: &Value,
) -> Result<(), ProtocolError> {
    use tokio::io::AsyncWriteExt;

    let buf = encode(value)?;
    stream.write_all(&buf).await?;
    stream.flush().await?;
    Ok(())
}

/// Async write a raw string as a length-prefixed frame to a tokio UnixStream.
pub async fn write_frame_raw_async(
    stream: &mut tokio::net::UnixStream,
    payload: &str,
) -> Result<(), ProtocolError> {
    use tokio::io::AsyncWriteExt;

    let buf = encode_raw(payload)?;
    stream.write_all(&buf).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn encode_decode_round_trip() {
        let original = json!({"type": "test", "value": 42});
        let encoded = encode(&original).unwrap();

        let mut cursor = std::io::Cursor::new(encoded);
        let decoded = read_frame(&mut cursor).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn rejects_oversized_payload() {
        let big = json!({"data": "x".repeat((MAX_PAYLOAD_SIZE + 1) as usize)});
        assert!(matches!(
            encode(&big),
            Err(ProtocolError::PayloadTooLarge(_))
        ));
    }

    #[test]
    fn rejects_zero_length_prefix() {
        let buf = vec![0u8; 4];
        let mut cursor = std::io::Cursor::new(buf);
        assert!(matches!(
            read_frame(&mut cursor),
            Err(ProtocolError::InvalidLengthPrefix)
        ));
    }
}
