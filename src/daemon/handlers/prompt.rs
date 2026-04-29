//! Handlers for the `prompt.*` method namespace.
//!
//! Slice 2: routes prompt execution through the shared Agent held in
//! [`DaemonState`]. Both buffered (`prompt.run`) and streaming
//! (`prompt.stream`) variants are supported. The streaming path spawns
//! a background task that drains `StreamEvent`s from the Agent and
//! forwards them as `StreamFrame`s over the socket.

use serde_json::json;
use tokio::sync::{mpsc, oneshot};

use crate::daemon::protocol::{ProtocolError, Response, StreamFrame};
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

// -- prompt.run --

pub async fn run(state: &DaemonState, id: String, input: &str) -> Response {
    let Some(agent_arc) = state.agent() else {
        return Response::error(
            id,
            ProtocolError {
                code: "AGENT_NOT_AVAILABLE".into(),
                message: "agent not initialized in daemon".into(),
                retryable: false,
            },
        );
    };
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
    input: String,
) -> (mpsc::Receiver<StreamFrame>, oneshot::Receiver<Response>) {
    let (frame_tx, frame_rx) = mpsc::channel(32);
    let (done_tx, done_rx) = oneshot::channel();

    let agent_arc = match state.agent().cloned() {
        Some(a) => a,
        None => {
            let _ = done_tx.send(Response::error(
                req_id,
                ProtocolError {
                    code: "AGENT_NOT_AVAILABLE".into(),
                    message: "agent not initialized in daemon".into(),
                    retryable: false,
                },
            ));
            return (frame_rx, done_rx);
        }
    };

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

pub async fn cancel(state: &DaemonState, id: String) -> Response {
    let Some(agent_arc) = state.agent() else {
        return Response::error(
            id,
            ProtocolError {
                code: "AGENT_NOT_AVAILABLE".into(),
                message: "agent not initialized in daemon".into(),
                retryable: false,
            },
        );
    };
    let agent = agent_arc.lock().await;
    agent.cancel_current_turn();
    Response::ok(id, json!({ "cancelled": true }))
}
