//! Daemon ↔ client wire protocol types (YYC-266 Slice 0, Task 0.2).
//!
//! These are the JSON shapes that ride inside length-delimited frames on
//! the Unix-domain socket. Frame I/O itself lands in Task 0.3 — this
//! module is purely the envelope structs plus a version-strict request
//! parser.
//!
//! ## Envelopes
//!
//! - [`Request`]: client → daemon. `{ version, id, session, method,
//!   params }`. The `session` field defaults to `"main"` when absent.
//! - [`Response`]: daemon → client. `{ version, id, result, error }`.
//!   Exactly one of `result` / `error` is non-null; the other is
//!   serialized as `null` (not omitted) so clients can discriminate by
//!   null-ness without ambiguity.
//! - [`StreamFrame`]: daemon → client streaming chunks and out-of-band
//!   push frames. `{ version, id, stream, data }`. `id` is `None` for
//!   push frames (e.g. `config_reloaded`, `session_evicted`) and is
//!   omitted from the JSON entirely in that case.
//! - [`ProtocolError`]: `{ code, message, retryable }`. Doubles as the
//!   error variant of [`Response`] and as a fail-fast result of
//!   [`parse_request_strict`].

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Wire protocol version. Bumped on any breaking change to the envelope
/// shapes or top-level method dispatch contract.
pub const PROTOCOL_VERSION: u32 = 1;

/// Client → daemon request envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Protocol version the client is speaking.
    pub version: u32,
    /// Client-assigned correlation id; echoed in [`Response::id`] and in
    /// any [`StreamFrame`]s emitted while servicing this request.
    pub id: String,
    /// Logical session bucket. Defaults to `"main"` when absent on the
    /// wire so single-session clients can stay terse.
    #[serde(default = "default_session")]
    pub session: String,
    /// Method name to dispatch (e.g. `"chat.send"`, `"config.reload"`).
    pub method: String,
    /// Method-specific parameters. Typed handlers re-deserialize this
    /// per method; the protocol layer keeps it opaque.
    #[serde(default)]
    pub params: serde_json::Value,
}

fn default_session() -> String {
    "main".into()
}

/// Daemon → client response envelope. Exactly one of `result` / `error`
/// is non-null; the other is `null` rather than omitted so JSON
/// consumers can discriminate without checking field presence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub version: u32,
    pub id: String,
    pub result: Option<serde_json::Value>,
    pub error: Option<ProtocolError>,
}

impl Response {
    /// Successful response. `result` is `Some(value)`, `error` is
    /// `None` (serialized as `null`).
    pub fn ok(id: String, result: serde_json::Value) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Failure response. `result` is `None` (serialized as `null`),
    /// `error` carries the [`ProtocolError`].
    pub fn error(id: String, err: ProtocolError) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id,
            result: None,
            error: Some(err),
        }
    }
}

/// Structured error carried inside [`Response`] (or returned from
/// [`parse_request_strict`] before dispatch).
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ProtocolError {
    /// Stable, machine-readable error code (e.g. `"VERSION_MISMATCH"`,
    /// `"INVALID_PARAMS"`, `"SESSION_NOT_FOUND"`). Clients dispatch on
    /// this; the message is human-facing.
    pub code: String,
    /// Human-readable detail. Safe to log; safe to surface to the user.
    pub message: String,
    /// Hint for clients: `true` means the same request might succeed if
    /// retried (e.g. transient I/O); `false` means don't bother.
    #[serde(default)]
    pub retryable: bool,
}

/// Streaming frame for incremental responses (`id = Some(req_id)`) and
/// out-of-band push frames (`id = None`, e.g. `config_reloaded`,
/// `session_evicted`).
///
/// The `id` field is omitted from the wire entirely when `None`, which
/// lets receivers cheaply distinguish push frames from chunked replies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamFrame {
    pub version: u32,
    /// Correlation id for chunked responses. `None` for push frames.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Stream channel name (e.g. `"text"`, `"tool_call"`,
    /// `"config_reloaded"`).
    pub stream: String,
    /// Channel-specific payload.
    pub data: serde_json::Value,
}

/// Decode a length-delimited request payload, rejecting any version
/// other than [`PROTOCOL_VERSION`]. The version check happens *before*
/// dispatch so unknown-version clients can't trigger handler logic.
pub fn parse_request_strict(bytes: &[u8]) -> Result<Request, ProtocolError> {
    let req: Request = serde_json::from_slice(bytes).map_err(|e| ProtocolError {
        code: "INVALID_PARAMS".into(),
        message: format!("malformed request: {e}"),
        retryable: false,
    })?;
    if req.version != PROTOCOL_VERSION {
        return Err(ProtocolError {
            code: "VERSION_MISMATCH".into(),
            message: format!("client v{}, daemon v{PROTOCOL_VERSION}", req.version),
            retryable: false,
        });
    }
    Ok(req)
}

/// Maximum size of one frame body, in bytes. Reads beyond this size
/// reject before allocating, so a malformed peer can't OOM us.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024; // 4 MiB

/// Write `body` as a length-delimited frame: u32 BE length prefix, then body.
pub async fn write_frame_bytes<W: AsyncWrite + Unpin>(
    w: &mut W,
    body: &[u8],
) -> std::io::Result<()> {
    let len: u32 = body
        .len()
        .try_into()
        .map_err(|_| std::io::Error::other("frame body exceeds u32::MAX"))?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(body).await?;
    w.flush().await
}

/// Read one length-delimited frame body. Rejects frames larger than
/// `MAX_FRAME_BYTES` before allocating.
pub async fn read_frame_bytes<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(std::io::Error::other(format!(
            "frame size {len} exceeds MAX_FRAME_BYTES ({MAX_FRAME_BYTES})"
        )));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(body)
}

/// Convenience: encode `req` as JSON, then frame it.
pub async fn write_request<W: AsyncWrite + Unpin>(w: &mut W, req: &Request) -> std::io::Result<()> {
    let body = serde_json::to_vec(req).map_err(std::io::Error::other)?;
    write_frame_bytes(w, &body).await
}

/// Convenience: read one frame, decode + version-check as a `Request`.
pub async fn read_request<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Request> {
    let body = read_frame_bytes(r).await?;
    parse_request_strict(&body).map_err(std::io::Error::other)
}

/// Convenience: encode `resp` as JSON, then frame it.
pub async fn write_response<W: AsyncWrite + Unpin>(
    w: &mut W,
    resp: &Response,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(resp).map_err(std::io::Error::other)?;
    write_frame_bytes(w, &body).await
}

/// Convenience: read one frame as a `Response` (no version check — clients
/// trust the daemon they auto-started).
pub async fn read_response<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Response> {
    let body = read_frame_bytes(r).await?;
    serde_json::from_slice(&body).map_err(std::io::Error::other)
}

/// Convenience: encode `frame` as JSON, then frame it.
pub async fn write_stream_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    frame: &StreamFrame,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(frame).map_err(std::io::Error::other)?;
    write_frame_bytes(w, &body).await
}

/// Convenience: read one frame as a `StreamFrame`.
pub async fn read_stream_frame<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<StreamFrame> {
    let body = read_frame_bytes(r).await?;
    serde_json::from_slice(&body).map_err(std::io::Error::other)
}
