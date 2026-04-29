//! Handlers for the `approval.*` method namespace.
//!
//! Slice 2 stubs: deferred to Phase 3 (multi-session daemon + TUI overlay).

use crate::daemon::protocol::{ProtocolError, Response};
use crate::daemon::state::DaemonState;

pub async fn pending(_state: &DaemonState, id: String) -> Response {
    Response::error(
        id,
        ProtocolError {
            code: "METHOD_NOT_IMPLEMENTED".into(),
            message: "approval.pending is not yet implemented".into(),
            retryable: false,
        },
    )
}

pub async fn respond(_state: &DaemonState, id: String, _decision: &str) -> Response {
    Response::error(
        id,
        ProtocolError {
            code: "METHOD_NOT_IMPLEMENTED".into(),
            message: "approval.respond is not yet implemented".into(),
            retryable: false,
        },
    )
}
