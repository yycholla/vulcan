//! Handlers for the `prompt.*` method namespace.
//!
//! Slice 3: each request envelope carries a `session` field; the
//! handler looks up the per-session warm Agent installed on that
//! `SessionState`. The `"main"` session gets its Agent installed by
//! the daemon boot path; other sessions are created without an Agent
//! and currently surface `AGENT_NOT_AVAILABLE` until lazy-build lands
//! in a later slice. Both buffered (`prompt.run`) and streaming
//! (`prompt.stream`) variants are supported.

use std::sync::Arc;

use serde_json::json;
use tokio::sync::{mpsc, oneshot};

use crate::daemon::protocol::{ProtocolError, Response, StreamFrame};
use crate::daemon::session::SessionState;
use crate::daemon::state::DaemonState;
use crate::provider::StreamEvent;

/// Map a daemon-internal `StreamEvent` to a wire `StreamFrame`.
fn stream_event_to_frame(req_id: &str, ev: StreamEvent) -> Option<StreamFrame> {
    match ev {
        StreamEvent::Text(chunk) => Some(StreamFrame {
            version: 1,
            id: Some(req_id.into()),
            stream: "text".into(),
            data: json!({ "chunk": chunk }),
        }),
        StreamEvent::Reasoning(chunk) => Some(StreamFrame {
            version: 1,
            id: Some(req_id.into()),
            stream: "reasoning".into(),
            data: json!({ "chunk": chunk }),
        }),
        StreamEvent::ToolCallStart {
            id: tool_id,
            name,
            args_summary,
        } => Some(StreamFrame {
            version: 1,
            id: Some(req_id.into()),
            stream: "tool_call_start".into(),
            data: json!({
                "tool_id": tool_id,
                "name": name,
                "args_summary": args_summary,
            }),
        }),
        StreamEvent::ToolCallEnd {
            id: tool_id,
            name,
            ok,
            result_meta,
            elapsed_ms,
            ..
        } => Some(StreamFrame {
            version: 1,
            id: Some(req_id.into()),
            stream: "tool_call_end".into(),
            data: json!({
                "tool_id": tool_id,
                "name": name,
                "ok": ok,
                "result_meta": result_meta,
                "elapsed_ms": elapsed_ms,
            }),
        }),
        // Done is the terminal marker; we don't forward it as a frame
        // because the final text comes via the join handle below.
        StreamEvent::Done(_) => None,
        StreamEvent::Error(e) => Some(StreamFrame {
            version: 1,
            id: Some(req_id.into()),
            stream: "error".into(),
            data: json!({ "reason": e }),
        }),
    }
}

/// Resolve the session referenced by a request envelope.
fn resolve_session(
    state: &DaemonState,
    session_id: &str,
) -> Result<Arc<SessionState>, ProtocolError> {
    state
        .sessions()
        .get(session_id)
        .ok_or_else(|| ProtocolError {
            code: "SESSION_NOT_FOUND".into(),
            message: format!("session '{session_id}' not found"),
            retryable: false,
        })
}

// -- prompt.run --

pub async fn run(state: Arc<DaemonState>, id: String, session_id: String, input: &str) -> Response {
    let sess = match resolve_session(&state, &session_id) {
        Ok(s) => s,
        Err(e) => return Response::error(id, e),
    };
    let agent_arc = match sess
        .ensure_agent_with_pool(state.config(), state.pool().cloned())
        .await
    {
        Ok(a) => a,
        Err(e) => {
            return Response::error(
                id,
                ProtocolError {
                    code: "AGENT_BUILD_FAILED".into(),
                    message: format!("agent build for session '{session_id}' failed: {e}"),
                    retryable: true,
                },
            );
        }
    };
    sess.touch();
    *sess.in_flight.lock() = true;
    let cancel_token = tokio_util::sync::CancellationToken::new();
    sess.set_agent_cancel(cancel_token.clone());
    let mut agent = agent_arc.lock().await;
    install_daemon_subagent_runner(&mut agent, Arc::clone(&state), &session_id);

    // GH issue #557: extensions with the `InputInterceptor` capability
    // can block or rewrite raw user input via `on_input` before slash
    // dispatch + turn execution. Block short-circuits here with the
    // hook's reason; Replace swaps the input on the wire path; Continue
    // forwards as-is.
    let effective_input: String = match agent.apply_on_input(input).await {
        crate::hooks::InputDecision::Continue => input.to_string(),
        crate::hooks::InputDecision::Replace(rewrite) => rewrite,
        crate::hooks::InputDecision::Block(reason) => {
            drop(agent);
            *sess.in_flight.lock() = false;
            sess.touch();
            return Response::error(
                id,
                ProtocolError {
                    code: "INPUT_BLOCKED".into(),
                    message: format!("input blocked by extension: {reason}"),
                    retryable: false,
                },
            );
        }
    };

    let result = agent
        .run_prompt_with_cancel(&effective_input, cancel_token)
        .await;
    drop(agent);
    *sess.in_flight.lock() = false;
    sess.touch();
    match result {
        Ok(output) => Response::ok(id, json!({ "text": output })),
        Err(e) => Response::error(
            id,
            ProtocolError {
                code: "PROMPT_RUN_FAILED".into(),
                message: format!("{e}"),
                retryable: false,
            },
        ),
    }
}

// -- prompt.stream --

/// Returns `(frame_rx, done_rx)` so the server can stream Text and
/// ToolCall events to the TUI while awaiting the final result.
pub fn stream(
    state: Arc<DaemonState>,
    req_id: String,
    session_id: String,
    input: String,
) -> (mpsc::Receiver<StreamFrame>, oneshot::Receiver<Response>) {
    let (frame_tx, frame_rx) = mpsc::channel(32);
    let (done_tx, done_rx) = oneshot::channel();

    let sess = match resolve_session(&state, &session_id) {
        Ok(s) => s,
        Err(e) => {
            let _ = done_tx.send(Response::error(req_id, e));
            return (frame_rx, done_rx);
        }
    };
    sess.touch();
    // Mark in_flight BEFORE spawning so daemon.status / any_in_flight()
    // see the busy state immediately, even if the spawned task hasn't
    // been scheduled yet. The spawned task clears it in every
    // completion path.
    *sess.in_flight.lock() = true;

    let rid = req_id.clone();
    let sess_for_task = sess.clone();
    // Lazy-build needs `&Config`, but the spawned task can't borrow
    // `state` so we clone the `Arc<Config>` out here.
    let config = Arc::new(state.config().clone());
    // Slice 3: clone the pool Arc out for the spawned task so the
    // child Agent build reuses the daemon's shared adapters.
    let pool_for_task = state.pool().cloned();
    let state_for_runner = Arc::clone(&state);
    tokio::spawn(async move {
        // Lazy-build the per-session Agent. Failure surfaces on the
        // done channel as AGENT_BUILD_FAILED; in_flight is cleared
        // before returning so daemon.status doesn't get stuck.
        let agent_arc = match sess_for_task
            .ensure_agent_with_pool(&config, pool_for_task)
            .await
        {
            Ok(a) => a,
            Err(e) => {
                *sess_for_task.in_flight.lock() = false;
                sess_for_task.touch();
                let _ = done_tx.send(Response::error(
                    rid.clone(),
                    ProtocolError {
                        code: "AGENT_BUILD_FAILED".into(),
                        message: format!(
                            "agent build for session '{}' failed: {e}",
                            sess_for_task.id
                        ),
                        retryable: true,
                    },
                ));
                return;
            }
        };

        let (event_tx, mut event_rx) = mpsc::channel::<StreamEvent>(32);
        let cancel_token = tokio_util::sync::CancellationToken::new();
        sess_for_task.set_agent_cancel(cancel_token.clone());

        // GH issue #557: run `on_input` interception before spawning
        // the streaming task. Block short-circuits with INPUT_BLOCKED;
        // Replace swaps the input forwarded to `run_prompt_stream_with_cancel`.
        let effective_input: String = {
            let agent = agent_arc.lock().await;
            match agent.apply_on_input(&input).await {
                crate::hooks::InputDecision::Continue => input.clone(),
                crate::hooks::InputDecision::Replace(rewrite) => rewrite,
                crate::hooks::InputDecision::Block(reason) => {
                    drop(agent);
                    *sess_for_task.in_flight.lock() = false;
                    sess_for_task.touch();
                    let _ = done_tx.send(Response::error(
                        rid.clone(),
                        ProtocolError {
                            code: "INPUT_BLOCKED".into(),
                            message: format!("input blocked by extension: {reason}"),
                            retryable: false,
                        },
                    ));
                    return;
                }
            }
        };

        // Clone the Arc for the prompt task so the guard lives inside it.
        let agent_arc2 = agent_arc.clone();
        let input2 = effective_input;
        let cancel2 = cancel_token.clone();
        let session_id_for_runner = session_id.clone();
        let prompt_task = tokio::spawn(async move {
            let mut agent = agent_arc2.lock().await;
            install_daemon_subagent_runner(&mut agent, state_for_runner, &session_id_for_runner);
            agent
                .run_prompt_stream_with_cancel(&input2, event_tx, cancel2)
                .await
        });

        // Forward events as StreamFrames.
        while let Some(ev) = event_rx.recv().await {
            if let Some(frame) = stream_event_to_frame(&rid, ev) {
                if frame_tx.send(frame).await.is_err() {
                    // TUI disconnected -- cancel the turn.
                    cancel_token.cancel();
                    break;
                }
            }
        }

        // Await final turn result (Result<String> — just text).
        let final_response = match prompt_task.await {
            Ok(Ok(final_text)) => Response::ok(rid.clone(), json!({ "text": final_text })),
            Ok(Err(e)) => Response::error(
                rid.clone(),
                ProtocolError {
                    code: "PROMPT_RUN_FAILED".into(),
                    message: format!("{e}"),
                    retryable: false,
                },
            ),
            Err(join_err) => Response::error(
                rid.clone(),
                ProtocolError {
                    code: "JOIN_ERROR".into(),
                    message: format!("{join_err}"),
                    retryable: false,
                },
            ),
        };

        // Clear in_flight in all 3 completion paths before signalling done.
        *sess_for_task.in_flight.lock() = false;
        sess_for_task.touch();
        let _ = done_tx.send(final_response);
    });

    (frame_rx, done_rx)
}

fn install_daemon_subagent_runner(
    agent: &mut crate::agent::Agent,
    state: Arc<DaemonState>,
    parent_session_id: &str,
) {
    let runner = Arc::new(crate::daemon::subagent::DaemonSubagentRunner::new(
        Arc::clone(&state),
    ));
    agent.install_subagent_runner(
        Arc::new(state.config().clone()),
        parent_session_id.to_string(),
        runner,
    );
}

// -- prompt.cancel --

/// Fire the session's per-turn cancellation token without locking the
/// AsyncMutex. This is critical: `prompt.stream` holds the AsyncMutex
/// for the entire turn, so any cancel path that takes the AsyncMutex
/// would deadlock against the very stream it's trying to cancel.
///
/// The token clone is captured at `set_agent` time and stashed on
/// `SessionState::agent_cancel`. Firing it cancels the in-flight turn;
/// the next `run_prompt` swap installs a fresh token.
///
/// The response includes `cancelled: <bool>` reflecting whether a turn
/// was actually in flight when cancel was called.
pub async fn cancel(state: &DaemonState, id: String, session_id: String) -> Response {
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
    let Some(token) = sess.agent_cancel() else {
        return Response::error(
            id,
            ProtocolError {
                code: "AGENT_NOT_AVAILABLE".into(),
                message: format!(
                    "session '{session_id}' has no agent yet; lazy-build deferred to Task 3.X"
                ),
                retryable: false,
            },
        );
    };
    let was_in_flight = *sess.in_flight.lock();
    token.cancel();
    sess.touch();
    Response::ok(id, json!({ "cancelled": was_in_flight }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::DaemonState;
    use std::sync::Arc;

    #[tokio::test]
    async fn run_returns_session_not_found_for_bogus_session() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        let resp = run(state, "r1".into(), "ghost".into(), "hi").await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, "SESSION_NOT_FOUND");
    }

    #[tokio::test]
    async fn run_attempts_lazy_build_for_session_without_agent() {
        // for_tests_minimal carries `Config::default()` which has no
        // valid provider config — Agent::builder.build() will fail.
        // The point is to verify ensure_agent is being called: the
        // error path now surfaces AGENT_BUILD_FAILED instead of the
        // pre-Task-3.3 AGENT_NOT_AVAILABLE.
        let state = Arc::new(DaemonState::for_tests_minimal());
        let resp = run(state, "r1".into(), "main".into(), "hi").await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, "AGENT_BUILD_FAILED");
    }

    #[tokio::test]
    async fn cancel_returns_session_not_found_for_bogus_session() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        let resp = cancel(&state, "r1".into(), "ghost".into()).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, "SESSION_NOT_FOUND");
    }

    #[tokio::test]
    async fn cancel_returns_agent_not_available_when_no_agent() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        // "main" has no agent in for_tests_minimal
        let resp = cancel(&state, "r1".into(), "main".into()).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, "AGENT_NOT_AVAILABLE");
    }

    #[tokio::test]
    async fn cancel_fires_token_and_reports_in_flight_state() {
        use tokio_util::sync::CancellationToken;
        let state = Arc::new(DaemonState::for_tests_minimal());
        let main = state.sessions().get("main").unwrap();
        let token = CancellationToken::new();
        *main.agent_cancel.lock() = Some(token.clone());
        *main.in_flight.lock() = true;

        let resp = cancel(&state, "r1".into(), "main".into()).await;
        let result = resp.result.expect("ok");
        assert_eq!(result["cancelled"], true);
        assert!(token.is_cancelled(), "cancel must fire agent_cancel token");
    }

    #[tokio::test]
    async fn cancel_reports_false_when_not_in_flight() {
        use tokio_util::sync::CancellationToken;
        let state = Arc::new(DaemonState::for_tests_minimal());
        let main = state.sessions().get("main").unwrap();
        let token = CancellationToken::new();
        *main.agent_cancel.lock() = Some(token.clone());
        // in_flight stays false (default)

        let resp = cancel(&state, "r1".into(), "main".into()).await;
        let result = resp.result.expect("ok");
        assert_eq!(result["cancelled"], false);
        assert!(token.is_cancelled(), "token still fires even when idle");
    }
}
