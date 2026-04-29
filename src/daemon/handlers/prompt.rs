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
use crate::daemon::session::{AgentHandle, SessionState};
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

/// Resolve `(session, agent_handle)` or surface a structured error.
/// Returns `AGENT_NOT_AVAILABLE` for sessions without an installed
/// agent (e.g. non-main sessions created via `session.create` before
/// lazy-build lands in Task 3.X).
fn resolve_session_with_agent(
    state: &DaemonState,
    session_id: &str,
) -> Result<(Arc<SessionState>, AgentHandle), ProtocolError> {
    let sess = resolve_session(state, session_id)?;
    let Some(agent) = sess.agent_arc() else {
        return Err(ProtocolError {
            code: "AGENT_NOT_AVAILABLE".into(),
            message: format!(
                "session '{session_id}' has no agent yet; lazy-build deferred to Task 3.X"
            ),
            retryable: false,
        });
    };
    Ok((sess, agent))
}

// -- prompt.run --

pub async fn run(state: &DaemonState, id: String, session_id: String, input: &str) -> Response {
    let (sess, agent_arc) = match resolve_session_with_agent(state, &session_id) {
        Ok(pair) => pair,
        Err(e) => return Response::error(id, e),
    };
    sess.touch();
    let mut agent = agent_arc.lock().await;
    match agent.run_prompt(input).await {
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
    state: &DaemonState,
    req_id: String,
    session_id: String,
    input: String,
) -> (mpsc::Receiver<StreamFrame>, oneshot::Receiver<Response>) {
    let (frame_tx, frame_rx) = mpsc::channel(32);
    let (done_tx, done_rx) = oneshot::channel();

    let (sess, agent_arc) = match resolve_session_with_agent(state, &session_id) {
        Ok(pair) => pair,
        Err(e) => {
            let _ = done_tx.send(Response::error(req_id, e));
            return (frame_rx, done_rx);
        }
    };
    sess.touch();

    let rid = req_id.clone();
    tokio::spawn(async move {
        let (event_tx, mut event_rx) = mpsc::channel::<StreamEvent>(32);
        let cancel_token = tokio_util::sync::CancellationToken::new();

        // Clone the Arc for the prompt task so the guard lives inside it.
        let agent_arc2 = agent_arc.clone();
        let input2 = input.clone();
        let cancel2 = cancel_token.clone();
        let prompt_task = tokio::spawn(async move {
            let mut agent = agent_arc2.lock().await;
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
        match prompt_task.await {
            Ok(Ok(final_text)) => {
                let _ = done_tx.send(Response::ok(rid.clone(), json!({ "text": final_text })));
            }
            Ok(Err(e)) => {
                let _ = done_tx.send(Response::error(
                    rid.clone(),
                    ProtocolError {
                        code: "PROMPT_RUN_FAILED".into(),
                        message: format!("{e}"),
                        retryable: false,
                    },
                ));
            }
            Err(join_err) => {
                let _ = done_tx.send(Response::error(
                    rid.clone(),
                    ProtocolError {
                        code: "JOIN_ERROR".into(),
                        message: format!("{join_err}"),
                        retryable: false,
                    },
                ));
            }
        }
    });

    (frame_rx, done_rx)
}

// -- prompt.cancel --

pub async fn cancel(state: &DaemonState, id: String, session_id: String) -> Response {
    let (sess, agent_arc) = match resolve_session_with_agent(state, &session_id) {
        Ok(pair) => pair,
        Err(e) => return Response::error(id, e),
    };
    sess.touch();
    let agent = agent_arc.lock().await;
    agent.cancel_current_turn();
    Response::ok(id, json!({ "cancelled": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::DaemonState;
    use std::sync::Arc;

    #[tokio::test]
    async fn run_returns_session_not_found_for_bogus_session() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        let resp = run(&state, "r1".into(), "ghost".into(), "hi").await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, "SESSION_NOT_FOUND");
    }

    #[tokio::test]
    async fn run_returns_agent_not_available_for_main_without_agent() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        // for_tests_minimal creates "main" with no Agent installed.
        let resp = run(&state, "r1".into(), "main".into(), "hi").await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, "AGENT_NOT_AVAILABLE");
    }
}
