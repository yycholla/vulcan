//! Handlers for the `session.*` method namespace.
//!
//! Slice 3 Task 3.1: `create`, `destroy`, and `list` are wired through
//! to the live `SessionMap`. GH #703: `search` / `resume` / `history`
//! read saved session bodies from the SQLite `SessionStore`.

use serde_json::json;
use std::sync::Arc;
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
    parent_session_id: Option<String>,
    lineage_label: Option<String>,
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

    match state.sessions().create_named_with_lineage(
        &new_id,
        parent_session_id.clone(),
        lineage_label.clone(),
    ) {
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

// -- search / resume / history (GH #703) --

/// Borrow the daemon-owned saved-session store.
fn pooled_session_store(
    state: &DaemonState,
) -> Result<Arc<crate::memory::SessionStore>, ProtocolError> {
    let Some(pool) = state.pool() else {
        return Err(ProtocolError {
            code: "SESSION_STORE_UNAVAILABLE".into(),
            message: "daemon RuntimeResourcePool is not installed; session store unavailable"
                .into(),
            retryable: true,
        });
    };

    if let Some(degraded) = pool
        .degraded_resources()
        .iter()
        .find(|resource| resource.component == "session_store")
    {
        return Err(ProtocolError {
            code: "SESSION_STORE_UNAVAILABLE".into(),
            message: format!("daemon session store is degraded: {}", degraded.message),
            retryable: true,
        });
    }

    Ok(pool.session_store())
}

/// Return saved session summaries from the daemon-owned session store.
pub async fn list_saved(state: &DaemonState, id: String, limit: usize) -> Response {
    let store = match pooled_session_store(state) {
        Ok(s) => s,
        Err(e) => return Response::error(id, e),
    };
    match store.list_sessions(limit).await {
        Ok(sessions) => Response::ok(id, json!({ "sessions": sessions })),
        Err(e) => Response::error(
            id,
            ProtocolError {
                code: "LIST_SESSIONS_FAILED".into(),
                message: format!("session list failed: {e}"),
                retryable: false,
            },
        ),
    }
}

/// FTS5 search across every saved session's messages.
pub async fn search(state: &DaemonState, id: String, query: &str, limit: usize) -> Response {
    let store = match pooled_session_store(state) {
        Ok(s) => s,
        Err(e) => return Response::error(id, e),
    };
    match store.search_messages(query, limit).await {
        Ok(hits) => Response::ok(
            id,
            json!({
                "hits": hits.iter().map(|h| json!({
                    "session_id": h.session_id,
                    "position": h.position,
                    "role": h.role,
                    "content": h.content,
                    "created_at": h.created_at,
                    "score": h.score,
                })).collect::<Vec<_>>(),
            }),
        ),
        Err(e) => Response::error(
            id,
            ProtocolError {
                code: "SEARCH_FAILED".into(),
                message: format!("session search failed: {e}"),
                retryable: false,
            },
        ),
    }
}

/// Rehydrate a saved session into a live one. Idempotent: resuming
/// an already-live session touches it and reports `already_live`.
/// The Agent lazy-loads the transcript from the store on its next
/// `prepare_turn`, so pointing its session id at the saved one is
/// the whole rehydration step.
pub async fn resume(state: &DaemonState, id: String, session_id: &str) -> Response {
    if let Some(sess) = state.sessions().get(session_id) {
        sess.touch();
        return Response::ok(
            id,
            json!({ "session_id": session_id, "already_live": true }),
        );
    }

    let store = match pooled_session_store(state) {
        Ok(s) => s,
        Err(e) => return Response::error(id, e),
    };
    match store.load_history(session_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Response::error(
                id,
                ProtocolError {
                    code: "SESSION_NOT_FOUND".into(),
                    message: format!("no saved session '{session_id}'"),
                    retryable: false,
                },
            );
        }
        Err(e) => {
            return Response::error(
                id,
                ProtocolError {
                    code: "SESSION_STORE_UNAVAILABLE".into(),
                    message: format!("could not read saved session: {e}"),
                    retryable: true,
                },
            );
        }
    }

    if let Err(e) = state
        .sessions()
        .create_named_with_lineage(session_id, None, None)
    {
        return Response::error(
            id,
            ProtocolError {
                code: "SESSION_EXISTS".into(),
                message: format!("{e}"),
                retryable: false,
            },
        );
    }
    let sess = state
        .sessions()
        .get(session_id)
        .expect("session registered above");
    let agent_arc = match sess.ensure_agent(&state.session_agent_assembler()).await {
        Ok(a) => a,
        Err(e) => {
            state.sessions().destroy(session_id);
            return Response::error(
                id,
                ProtocolError {
                    code: "AGENT_BUILD_FAILED".into(),
                    message: format!("agent build for resumed session failed: {e}"),
                    retryable: true,
                },
            );
        }
    };
    if let Err(e) = agent_arc.lock().await.resume_session(session_id).await {
        state.sessions().destroy(session_id);
        return Response::error(
            id,
            ProtocolError {
                code: "RESUME_FAILED".into(),
                message: format!("could not resume '{session_id}': {e}"),
                retryable: false,
            },
        );
    }
    Response::ok(
        id,
        json!({ "session_id": session_id, "already_live": false }),
    )
}

/// Return a saved session's full message log.
pub async fn history(state: &DaemonState, id: String, session_id: &str) -> Response {
    let store = match pooled_session_store(state) {
        Ok(s) => s,
        Err(e) => return Response::error(id, e),
    };
    match store.load_history(session_id).await {
        Ok(Some(messages)) => match serde_json::to_value(&messages) {
            Ok(v) => Response::ok(id, json!({ "session_id": session_id, "messages": v })),
            Err(e) => Response::error(
                id,
                ProtocolError {
                    code: "HISTORY_ENCODE_FAILED".into(),
                    message: format!("{e}"),
                    retryable: false,
                },
            ),
        },
        Ok(None) => Response::error(
            id,
            ProtocolError {
                code: "SESSION_NOT_FOUND".into(),
                message: format!("no saved session '{session_id}'"),
                retryable: false,
            },
        ),
        Err(e) => Response::error(
            id,
            ProtocolError {
                code: "SESSION_STORE_UNAVAILABLE".into(),
                message: format!("could not read saved session: {e}"),
                retryable: true,
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::DaemonState;
    use crate::provider::Message;
    use crate::runtime_pool::RuntimeResourcePool;
    use std::sync::Arc;

    #[tokio::test]
    async fn create_with_explicit_id_succeeds() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        let resp = create(&state, "r1".into(), Some("foo".into()), None, None, None).await;
        let result = resp.result.expect("ok");
        assert_eq!(result["session_id"], "foo");
        assert!(state.sessions().get("foo").is_some());
    }

    #[tokio::test]
    async fn create_without_id_generates_uuid() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        let resp = create(&state, "r1".into(), None, None, None, None).await;
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
        create(&state, "r1".into(), Some("foo".into()), None, None, None).await;
        let resp = create(&state, "r2".into(), Some("foo".into()), None, None, None).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, "SESSION_EXISTS");
    }

    #[tokio::test]
    async fn create_rejects_main() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        let resp = create(&state, "r1".into(), Some("main".into()), None, None, None).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, "SESSION_EXISTS");
    }

    #[tokio::test]
    async fn destroy_removes_session() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        create(&state, "r1".into(), Some("foo".into()), None, None, None).await;
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

    // GH #703: resume of a live session is idempotent and never
    // touches the on-disk store.
    #[tokio::test]
    async fn resume_live_session_reports_already_live() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        create(&state, "r1".into(), Some("foo".into()), None, None, None).await;
        let resp = resume(&state, "r2".into(), "foo").await;
        let result = resp.result.expect("ok");
        assert_eq!(result["already_live"], true);
        assert_eq!(result["session_id"], "foo");
    }

    #[tokio::test]
    async fn history_requires_daemon_pool_session_store() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        let resp = history(&state, "r1".into(), "missing").await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, "SESSION_STORE_UNAVAILABLE");
        assert!(err.message.contains("RuntimeResourcePool"));
    }

    #[tokio::test]
    async fn history_reads_from_daemon_pool_session_store() {
        let pool = Arc::new(RuntimeResourcePool::for_tests().await);
        let session_id = uuid::Uuid::new_v4().to_string();
        pool.session_store()
            .save_messages(
                &session_id,
                &[Message::User {
                    content: "pooled daemon history".into(),
                }],
            )
            .await
            .unwrap();
        let state = Arc::new(DaemonState::for_tests_minimal().with_pool(pool));

        let resp = history(&state, "r1".into(), &session_id).await;
        let messages = resp.result.expect("ok")["messages"].clone();
        assert_eq!(messages[0]["content"], "pooled daemon history");
    }

    #[tokio::test]
    async fn list_saved_reads_from_daemon_pool_session_store() {
        let pool = Arc::new(RuntimeResourcePool::for_tests().await);
        let session_id = uuid::Uuid::new_v4().to_string();
        pool.session_store()
            .save_messages(
                &session_id,
                &[Message::User {
                    content: "saved session preview".into(),
                }],
            )
            .await
            .unwrap();
        let state = Arc::new(DaemonState::for_tests_minimal().with_pool(pool));

        let resp = list_saved(&state, "r1".into(), 5).await;
        let sessions = resp.result.expect("ok")["sessions"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(sessions[0]["id"], session_id);
        assert_eq!(sessions[0]["preview"], "saved session preview");
    }

    #[tokio::test]
    async fn search_reads_from_daemon_pool_session_store() {
        let pool = Arc::new(RuntimeResourcePool::for_tests().await);
        let session_id = uuid::Uuid::new_v4().to_string();
        pool.session_store()
            .save_messages(
                &session_id,
                &[Message::User {
                    content: "uniquepooledqueryneedle".into(),
                }],
            )
            .await
            .unwrap();
        let state = Arc::new(DaemonState::for_tests_minimal().with_pool(pool));

        let resp = search(&state, "r1".into(), "uniquepooledqueryneedle", 10).await;
        let result = resp.result.expect("ok");
        let hits = result["hits"].as_array().expect("hits");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0]["session_id"], session_id);
    }

    #[tokio::test]
    async fn degraded_pool_session_store_fails_clearly() {
        let pool = Arc::new(
            RuntimeResourcePool::for_tests_degraded("session_store", "sqlite unavailable").await,
        );
        let state = Arc::new(DaemonState::for_tests_minimal().with_pool(pool));

        let resp = search(&state, "r1".into(), "anything", 10).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, "SESSION_STORE_UNAVAILABLE");
        assert!(err.message.contains("sqlite unavailable"));
    }

    // GH #703: history of a session that was never saved is a typed
    // SESSION_NOT_FOUND, not METHOD_NOT_IMPLEMENTED.
    #[tokio::test]
    async fn history_missing_session_not_found() {
        let pool = Arc::new(RuntimeResourcePool::for_tests().await);
        let state = Arc::new(DaemonState::for_tests_minimal().with_pool(pool));
        let ghost = uuid::Uuid::new_v4().to_string();
        let resp = history(&state, "r1".into(), &ghost).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, "SESSION_NOT_FOUND");
        assert_ne!(err.code, "METHOD_NOT_IMPLEMENTED");
    }

    // GH #703: resume of an unknown saved session is typed too.
    #[tokio::test]
    async fn resume_missing_session_not_found() {
        let pool = Arc::new(RuntimeResourcePool::for_tests().await);
        let state = Arc::new(DaemonState::for_tests_minimal().with_pool(pool));
        let ghost = uuid::Uuid::new_v4().to_string();
        let resp = resume(&state, "r1".into(), &ghost).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, "SESSION_NOT_FOUND");
        assert!(state.sessions().get(&ghost).is_none());
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
        create(&state, "r1".into(), Some("a".into()), None, None, None).await;
        create(&state, "r2".into(), Some("b".into()), None, None, None).await;
        let resp = list(&state, "r3".into()).await;
        let count = resp.result.unwrap()["sessions"].as_array().unwrap().len();
        assert_eq!(count, 3, "main + a + b");
    }

    #[tokio::test]
    async fn create_child_session_records_parent_lineage() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        let resp = create(
            &state,
            "r1".into(),
            Some("child-1".into()),
            None,
            Some("main".into()),
            Some("spawn_subagent: review worker".into()),
        )
        .await;
        assert!(
            resp.error.is_none(),
            "create must succeed: {:?}",
            resp.error
        );

        let listed = list(&state, "r2".into()).await;
        let result = listed.result.expect("list ok");
        let child = result["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .find(|s| s["id"] == "child-1")
            .expect("child descriptor");
        assert_eq!(child["parent_session_id"], "main");
        assert_eq!(child["lineage_label"], "spawn_subagent: review worker");
    }

    #[tokio::test]
    async fn destroy_fires_agent_cancel_token_when_present() {
        use tokio_util::sync::CancellationToken;

        let state = Arc::new(DaemonState::for_tests_minimal());
        create(&state, "r1".into(), Some("foo".into()), None, None, None).await;

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
        create(&state, "r1".into(), Some("foo".into()), None, None, None).await;
        // No agent_cancel installed — destroy must still succeed.
        let resp = destroy(&state, "r2".into(), "foo".into()).await;
        assert_eq!(resp.result.unwrap()["ok"], true);
        assert!(state.sessions().get("foo").is_none());
    }
}
