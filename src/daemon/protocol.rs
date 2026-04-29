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
