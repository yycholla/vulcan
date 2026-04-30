//! Client-side error type. Maps daemon protocol errors and transport
//! failures into a typed surface.
//!
//! Distinct variants matter at the call site: callers need to tell
//! "daemon isn't there" (auto-start should retry) from "daemon answered
//! with a structured error" (caller decides per-error-code) from "raw
//! I/O blew up" (probably a bug). Keeping [`ProtocolError`] preserved
//! verbatim inside [`ClientError::Daemon`] also lets the caller
//! programmatically dispatch on `code` instead of regex-matching a
//! flattened string.

use crate::daemon::protocol::ProtocolError;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// `connect(2)` to the Unix socket failed (no daemon listening, or
    /// `EACCES` / `ENOENT`). [`super::Client::connect_or_autostart`]
    /// catches this and runs the auto-start path.
    #[error("connection refused: {0}")]
    ConnectionRefused(std::io::Error),

    /// Auto-start spawned `vulcan daemon start` but the socket never
    /// came up before the polling deadline elapsed.
    #[error("daemon failed to auto-start within {timeout_secs}s")]
    AutostartFailed { timeout_secs: u64 },

    /// Generic transport-level I/O — a write/read on the connected
    /// socket failed mid-call.
    #[error("transport I/O failure: {0}")]
    Io(#[from] std::io::Error),

    /// JSON decode of a frame body failed. Indicates a daemon bug or
    /// protocol-version drift; not user-recoverable.
    #[error("protocol decode failure: {0}")]
    Decode(#[from] serde_json::Error),

    /// A low-level protocol or framing error occurred during streaming.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// Daemon answered with a structured [`ProtocolError`]. The full
    /// error is preserved so callers can match on `code`.
    #[error("daemon error [{}]: {}", .0.code, .0.message)]
    Daemon(ProtocolError),
}

impl From<ProtocolError> for ClientError {
    fn from(err: ProtocolError) -> Self {
        ClientError::Daemon(err)
    }
}

pub type ClientResult<T> = std::result::Result<T, ClientError>;
