//! Handlers for the `session.*` method namespace.
//!
//! Slice 3 Task 3.1: `create`, `destroy`, and `list` are wired through
//! to the live `SessionMap`. `search` / `resume` / `history` remain
//! stubbed pending later slices that load saved session bodies from
//! disk.

use serde_json::json;
use uuid::Uuid;

use crate::daemon::protocol::{ProtocolError, Response};
use crate::daemon::state::DaemonState;

// -- session.create --

/// Create a new session. If `id` is `None`, a UUID v4 is generated.
/// `resume_from` is accepted but currently ignored (lazy-load of
/// historical session bodies is deferred to Slice 4).
pub async fn create(
    state: &DaemonState,
    id: String,
    requested_id: Option<String>,
    resume_from: Option<String>,
) -> Response {
    // resume_from is reserved for a later slice that re-hydrates a
    // historical session into the new one. For now we just accept
    // and ignore the field so callers can start passing it.
    let _ = resume_from;

    let new_id = requested_id.unwrap_or_else(|| Uuid::new_v4().to_string());

    if new_id == "main" {
        return Response::error(
            id,
            ProtocolError {
                code: "SESSION_EXISTS".into(),
                message: "session 'main' is reserved".into(),
                retryable: false,
            },
        );
    }

    match state.sessions().create_named(&new_id) {
        Ok(_) => Response::ok(id, json!({ "session_id": new_id })),
        Err(_) => Response::error(
            id,
            ProtocolError {
                code: "SESSION_EXISTS".into(),
                message: format!("session '{new_id}' already exists"),
                retryable: false,
            },
        ),
    }
}

// -- session.destroy --

/// Remove a session from the map. Refuses to destroy `"main"`.
/// Cancels the session's in-flight cancel token as a best-effort
/// cleanup of any work currently bound to it.
pub async fn destroy(state: &DaemonState, id: String, session_id: String) -> Response {
    if session_id == "main" {
        return Response::error(
            id,
            ProtocolError {
                code: "CANNOT_DESTROY_MAIN".into(),
                message: "the 'main' session cannot be destroyed".into(),
                retryable: false,
            },
        );
    }

    let Some(sess) = state.sessions().get(&session_id) else {
        return Response::error(
            id,
            ProtocolError {
                code: "SESSION_NOT_FOUND".into(),
                message: format!("session '{session_id}' not found"),
                retryable: false,
            },
        );
    };

    // Best-effort: signal cancellation to anything bound to this
    // session before we drop the map entry. The actual Agent (if
    // any) will be dropped when the last `AgentHandle` Arc is
    // released.
    //
    // Fire BOTH the session-level cancel token and the agent's per-turn
    // cancel token. The session-level token is for any work bound to
    // the session lifecycle (e.g. future watchers); the agent_cancel
    // token signals an in-flight turn to stop at its next cancellation
    // check. Without firing agent_cancel, a mid-turn agent loop would
    // keep running and eventually try to send Response::ok into a
    // oneshot::Sender whose receiver has been dropped.
    sess.cancel.cancel();
    if let Some(token) = sess.agent_cancel() {
        token.cancel();
    }
    state.sessions().destroy(&session_id);

    Response::ok(id, json!({ "ok": true }))
}

// -- session.list --

/// Return one descriptor per live session.
pub async fn list(state: &DaemonState, id: String) -> Response {
    Response::ok(
        id,
        json!({
            "sessions": state.session_descriptors(),
        }),
    )
}

// -- search / resume / history (deferred) --

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::DaemonState;
    use std::sync::Arc;

    #[tokio::test]
    async fn create_with_explicit_id_succeeds() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        let resp = create(&state, "r1".into(), Some("foo".into()), None).await;
        let result = resp.result.expect("ok");
        assert_eq!(result["session_id"], "foo");
        assert!(state.sessions().get("foo").is_some());
    }

    #[tokio::test]
    async fn create_without_id_generates_uuid() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        let resp = create(&state, "r1".into(), None, None).await;
        let result = resp.result.expect("ok");
        let id = result["session_id"].as_str().unwrap();
        assert!(
            uuid::Uuid::parse_str(id).is_ok(),
            "must be valid UUID, got {id}"
        );
    }

    #[tokio::test]
    async fn create_rejects_duplicate() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        create(&state, "r1".into(), Some("foo".into()), None).await;
        let resp = create(&state, "r2".into(), Some("foo".into()), None).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, "SESSION_EXISTS");
    }

    #[tokio::test]
    async fn create_rejects_main() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        let resp = create(&state, "r1".into(), Some("main".into()), None).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, "SESSION_EXISTS");
    }

    #[tokio::test]
    async fn destroy_removes_session() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        create(&state, "r1".into(), Some("foo".into()), None).await;
        let resp = destroy(&state, "r2".into(), "foo".into()).await;
        assert_eq!(resp.result.unwrap()["ok"], true);
        assert!(state.sessions().get("foo").is_none());
    }

    #[tokio::test]
    async fn destroy_main_rejected() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        let resp = destroy(&state, "r1".into(), "main".into()).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, "CANNOT_DESTROY_MAIN");
    }

    #[tokio::test]
    async fn destroy_missing_session_rejected() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        let resp = destroy(&state, "r1".into(), "ghost".into()).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, "SESSION_NOT_FOUND");
    }

    #[tokio::test]
    async fn list_includes_main_after_boot() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        let resp = list(&state, "r1".into()).await;
        let result = resp.result.expect("ok");
        let sessions = result["sessions"].as_array().unwrap();
        assert!(sessions.iter().any(|s| s["id"] == "main"));
    }

    #[tokio::test]
    async fn list_reflects_create_destroy() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        create(&state, "r1".into(), Some("a".into()), None).await;
        create(&state, "r2".into(), Some("b".into()), None).await;
        let resp = list(&state, "r3".into()).await;
        let count = resp.result.unwrap()["sessions"].as_array().unwrap().len();
        assert_eq!(count, 3, "main + a + b");
    }

    #[tokio::test]
    async fn destroy_fires_agent_cancel_token_when_present() {
        use tokio_util::sync::CancellationToken;

        let state = Arc::new(DaemonState::for_tests_minimal());
        create(&state, "r1".into(), Some("foo".into()), None).await;

        // Install a fake cancel token directly on the session for the test.
        let foo = state.sessions().get("foo").unwrap();
        let token = CancellationToken::new();
        *foo.agent_cancel.lock() = Some(token.clone());

        destroy(&state, "r2".into(), "foo".into()).await;
        assert!(token.is_cancelled(), "destroy must fire agent_cancel token");
    }

    #[tokio::test]
    async fn destroy_without_agent_cancel_still_succeeds() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        create(&state, "r1".into(), Some("foo".into()), None).await;
        // No agent_cancel installed — destroy must still succeed.
        let resp = destroy(&state, "r2".into(), "foo".into()).await;
        assert_eq!(resp.result.unwrap()["ok"], true);
        assert!(state.sessions().get("foo").is_none());
    }
}
