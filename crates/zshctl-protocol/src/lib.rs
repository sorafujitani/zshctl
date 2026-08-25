use serde::{Deserialize, Serialize};
use std::io;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

pub const PROTOCOL_VERSION: u16 = 1;
pub const DEFAULT_MAX_FRAME_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub protocol_version: u16,
    pub request_id: Uuid,
    pub operation: Operation,
    pub shell_session_id: String,
    pub working_directory: String,
    #[serde(default)]
    pub environment: std::collections::BTreeMap<String, String>,
}

impl Request {
    pub fn new(
        operation: Operation,
        shell_session_id: impl Into<String>,
        working_directory: impl Into<String>,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::new_v4(),
            operation,
            shell_session_id: shell_session_id.into(),
            working_directory: working_directory.into(),
            environment: Default::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "name", content = "payload", rename_all = "snake_case")]
pub enum Operation {
    Health,
    Capabilities,
    Shutdown,
    Cancel {
        request_id: Uuid,
    },
    Feature {
        name: String,
        payload: serde_json::Value,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub protocol_version: u16,
    pub request_id: Uuid,
    #[serde(flatten)]
    pub result: ResponseResult,
}

impl Response {
    pub fn success(request_id: Uuid, value: serde_json::Value) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            result: ResponseResult::Success { value },
        }
    }

    pub fn error(request_id: Uuid, error: ProtocolError) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            result: ResponseResult::Error { error },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResponseResult {
    Success { value: serde_json::Value },
    Error { error: ProtocolError },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Cancelled,
    Validation,
    Timeout,
    Internal,
    Incompatible,
    UnknownOperation,
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    pub pid: u32,
    pub build_identity: String,
    pub protocol_version: u16,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("frame of {actual} bytes exceeds the {maximum} byte limit")]
    TooLarge { actual: usize, maximum: usize },
    #[error("malformed JSON frame: {0}")]
    Malformed(#[from] serde_json::Error),
}

pub async fn read_frame<R, T>(reader: &mut R, maximum: usize) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: for<'de> Deserialize<'de>,
{
    let length = reader.read_u32().await? as usize;
    if length > maximum {
        return Err(FrameError::TooLarge {
            actual: length,
            maximum,
        });
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub async fn write_frame<W, T>(writer: &mut W, value: &T, maximum: usize) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > maximum {
        return Err(FrameError::TooLarge {
            actual: bytes.len(),
            maximum,
        });
    }
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[tokio::test]
    async fn frame_round_trip_preserves_request() {
        let request = Request::new(Operation::Health, "shell-1", "/tmp");
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &request, DEFAULT_MAX_FRAME_SIZE)
            .await
            .unwrap();
        let decoded: Request = read_frame(&mut buffer.as_slice(), DEFAULT_MAX_FRAME_SIZE)
            .await
            .unwrap();
        assert_eq!(decoded, request);
    }

    #[tokio::test]
    async fn oversized_header_is_rejected_before_allocation() {
        let frame = ((DEFAULT_MAX_FRAME_SIZE + 1) as u32).to_be_bytes().to_vec();
        let error = read_frame::<_, Request>(&mut frame.as_slice(), DEFAULT_MAX_FRAME_SIZE)
            .await
            .unwrap_err();
        assert!(matches!(error, FrameError::TooLarge { .. }));
    }

    #[tokio::test]
    async fn truncated_and_malformed_frames_are_bounded_errors() {
        let mut truncated = 10_u32.to_be_bytes().to_vec();
        truncated.extend_from_slice(b"abc");
        let error = read_frame::<_, Request>(&mut truncated.as_slice(), DEFAULT_MAX_FRAME_SIZE)
            .await
            .unwrap_err();
        assert!(matches!(error, FrameError::Io(_)));

        let mut malformed = 1_u32.to_be_bytes().to_vec();
        malformed.push(b'{');
        let error = read_frame::<_, Request>(&mut malformed.as_slice(), DEFAULT_MAX_FRAME_SIZE)
            .await
            .unwrap_err();
        assert!(matches!(error, FrameError::Malformed(_)));
    }

    #[test]
    fn request_has_stable_golden_serialization() {
        let request = Request {
            protocol_version: 1,
            request_id: Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
            operation: Operation::Health,
            shell_session_id: "shell-1".into(),
            working_directory: "/work".into(),
            environment: Default::default(),
        };
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "protocol_version": 1,
                "request_id": "00000000-0000-0000-0000-000000000001",
                "operation": { "name": "health" },
                "shell_session_id": "shell-1",
                "working_directory": "/work"
                ,"environment": {}
            })
        );
    }

    #[test]
    fn unknown_operation_deserializes_to_a_bounded_variant() {
        let request: Request = serde_json::from_value(serde_json::json!({
            "protocol_version": 1,
            "request_id": "00000000-0000-0000-0000-000000000001",
            "operation": { "name": "future_operation" },
            "shell_session_id": "shell-1",
            "working_directory": "/tmp"
        }))
        .unwrap();
        assert!(matches!(request.operation, Operation::Unknown));
    }

    proptest! {
        #[test]
        fn arbitrary_session_and_directory_round_trip(
            session in ".{0,256}",
            directory in ".{0,256}"
        ) {
            let request = Request::new(Operation::Health, session, directory);
            let bytes = serde_json::to_vec(&request).unwrap();
            let decoded: Request = serde_json::from_slice(&bytes).unwrap();
            prop_assert_eq!(decoded, request);
        }
    }
}
