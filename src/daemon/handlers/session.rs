//! Handlers for the `session.*` method namespace.
//!
//! Slice 2 stubs: deferred to Phase 3 (multi-session daemon).

use crate::daemon::protocol::{ProtocolError, Response};
use crate::daemon::state::DaemonState;

pub async fn list(state: &DaemonState, id: String) -> Response {
    let _ = state;
    Response::error(
        id,
        ProtocolError {
            code: "METHOD_NOT_IMPLEMENTED".into(),
            message: "session.list is not yet implemented".into(),
            retryable: false,
        },
    )
}

pub async fn search(state: &DaemonState, id: String, _query: &str, _limit: usize) -> Response {
    let _ = state;
    Response::error(
        id,
        ProtocolError {
            code: "METHOD_NOT_IMPLEMENTED".into(),
            message: "session.search is not yet implemented".into(),
            retryable: false,
        },
    )
}

pub async fn resume(state: &DaemonState, id: String, _session_id: &str) -> Response {
    let _ = state;
    Response::error(
        id,
        ProtocolError {
            code: "METHOD_NOT_IMPLEMENTED".into(),
            message: "session.resume is not yet implemented".into(),
            retryable: false,
        },
    )
}

pub async fn history(state: &DaemonState, id: String, _session_id: &str) -> Response {
    let _ = state;
    Response::error(
        id,
        ProtocolError {
            code: "METHOD_NOT_IMPLEMENTED".into(),
            message: "session.history is not yet implemented".into(),
            retryable: false,
        },
    )
}
