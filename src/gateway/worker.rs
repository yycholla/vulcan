//! Lane worker — pulls a claimed inbound row, drives the prompt through
//! the daemon's `prompt.stream` RPC, accumulates the streamed text into a
//! reply, and enqueues a single outbound message.
//!
//! Slice 3 Task 3.4 changed the shape: the gateway no longer owns the
//! Agent. Each lane maps to a daemon session id (via [`DaemonLaneRouter`])
//! while the gateway owns one shared daemon client. The worker calls
//! `prompt.stream` against that session and drains the stream into a
//! buffered reply. Edit-in-place via `StreamRenderer` is deferred to a
//! follow-up that lands a daemon → gateway streaming-render bridge.
//!
//! The lane router (`mod.rs`'s inbound dispatcher) hands one inbound row
//! at a time to `process_one`. RPC failures (transport, daemon error)
//! are mapped to `mark_failed`; the worker loop survives so the next row
//! gets a fresh shot.

use std::sync::Arc;

use crate::daemon::protocol::StreamFrame;
use crate::gateway::commands::{CommandDispatcher, DispatchCtx};
use crate::gateway::daemon_client::GatewayDaemonClient;
use crate::gateway::lane::LaneKey;
use crate::gateway::lane_router::DaemonLaneRouter;
use crate::gateway::queue::{InboundQueue, InboundRow, OutboundQueue};
use crate::gateway::render_registry::RenderRegistry;
use crate::platform::{OutboundMessage, PlatformCapabilities};

/// Drive one inbound row through the daemon and enqueue the reply.
///
/// Steps:
/// 1. Slash-command shortcut: route `/`-prefixed text through
///    [`CommandDispatcher`] first. Any handled command produces an
///    atomic outbound row and skips the prompt.* RPC entirely.
/// 2. Look up (or create) the daemon session id for the row's lane.
/// 3. Reuse the gateway-owned daemon client and call `prompt.stream`
///    against that session id with the row's text.
/// 4. Drain `text` chunks from the stream into a reply buffer; await
///    the final response.
/// 5. On success: enqueue the reply as a single outbound message and
///    mark the inbound row done.
/// 6. On failure: mark the inbound row failed.
pub async fn process_one(
    row: InboundRow,
    lane_router: &DaemonLaneRouter,
    daemon_client: &GatewayDaemonClient,
    inbound_queue: &InboundQueue,
    // YYC-266 Slice 3 Task 3.4: outbound rows now flow through
    // `inbound_queue.complete_with_outbound`, which owns the atomic
    // outbound write internally. The OutboundQueue handle stays in
    // the signature so a future stream-render bridge can edit-in-place
    // without a churn-y signature change; keep the binding live.
    _outbound_queue: &Arc<OutboundQueue>,
    _render_registry: &Arc<RenderRegistry>,
    _platform_caps: PlatformCapabilities,
    commands: &CommandDispatcher,
) -> anyhow::Result<()> {
    let lane = LaneKey {
        platform: row.platform.clone(),
        chat_id: row.chat_id.clone(),
    };

    // YYC-18 PR-2c: route slash commands through CommandDispatcher
    // first. On Some(reply), enqueue an atomic outbound row and mark
    // inbound done in one transaction. On None, fall through to the
    // daemon prompt path below.
    match commands
        .dispatch(
            &row.text,
            DispatchCtx {
                lane: &lane,
                user_id: &row.user_id,
                lane_router,
                daemon_client,
                body: "",
            },
        )
        .await
    {
        Ok(Some(reply)) => {
            let id = row.id;
            inbound_queue
                .complete_with_outbound(
                    id,
                    OutboundMessage {
                        platform: row.platform,
                        chat_id: row.chat_id,
                        text: reply,
                        attachments: vec![],
                        reply_to: None,
                        edit_target: None,
                        turn_id: None,
                    },
                )
                .await?;
            return Ok(());
        }
        Ok(None) => {}
        Err(e) => {
            let err_str = e.to_string();
            inbound_queue.mark_failed(row.id, &err_str).await?;
            return Err(e);
        }
    }

    // Drive the prompt through the daemon. Failures here surface as
    // `mark_failed` on the inbound row; the dispatcher loop survives
    // and picks up the next row.
    let result = run_prompt_via_daemon(&lane, &row.text, lane_router, daemon_client).await;

    match result {
        Ok(reply_text) => {
            inbound_queue
                .complete_with_outbound(
                    row.id,
                    OutboundMessage {
                        platform: row.platform,
                        chat_id: row.chat_id,
                        text: reply_text,
                        attachments: vec![],
                        reply_to: None,
                        edit_target: None,
                        turn_id: None,
                    },
                )
                .await?;
            Ok(())
        }
        Err(e) => {
            let err_str = e.to_string();
            inbound_queue.mark_failed(row.id, &err_str).await?;
            Err(e)
        }
    }
}

/// Ensure the daemon session exists for `lane` and stream a
/// `prompt.stream` request through the gateway-owned shared client.
/// Drains text chunks into the returned reply string. The final
/// response's `text` field (if any) takes precedence over the
/// accumulated chunks so we don't double-emit.
async fn run_prompt_via_daemon(
    lane: &LaneKey,
    input: &str,
    lane_router: &DaemonLaneRouter,
    daemon_client: &GatewayDaemonClient,
) -> anyhow::Result<String> {
    let client = daemon_client
        .shared_client()
        .await
        .map_err(|e| anyhow::anyhow!("client connect: {e}"))?;

    let session_id = lane_router
        .ensure_session(lane, &client)
        .await
        .map_err(|e| anyhow::anyhow!("ensure_session: {e}"))?;

    let mut stream = client
        .call_stream_at_session(
            &session_id,
            "prompt.stream",
            serde_json::json!({ "text": input }),
        )
        .await
        .map_err(|e| anyhow::anyhow!("prompt.stream call: {e}"))?;

    let mut reply = String::new();
    while let Some(frame) = stream.frames.recv().await {
        if let Some(chunk) = extract_text_chunk(&frame) {
            reply.push_str(&chunk);
        }
        // Other frame kinds (tool_call_start/end, reasoning, error)
        // are not surfaced through the buffered reply path. A future
        // bridge will pump them through StreamRenderer for
        // edit-in-place output; for now we just discard.
    }

    let final_response = stream
        .done
        .await
        .map_err(|_| anyhow::anyhow!("daemon dropped completion sender"))?
        .map_err(|e| anyhow::anyhow!("stream completion: {e}"))?;

    if let Some(err) = final_response.error {
        anyhow::bail!("daemon prompt.stream error [{}]: {}", err.code, err.message);
    }

    // Prefer the final `text` field if the daemon returned it; fall
    // back to the streamed accumulation otherwise.
    if let Some(result) = final_response.result {
        if let Some(final_text) = result.get("text").and_then(|v| v.as_str()) {
            return Ok(final_text.to_string());
        }
    }
    Ok(reply)
}

/// Pull a `text` chunk out of a `StreamFrame` if the frame is on the
/// `text` channel. Returns `None` for any other channel.
fn extract_text_chunk(frame: &StreamFrame) -> Option<String> {
    if frame.stream != "text" {
        return None;
    }
    frame
        .data
        .get("chunk")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    //! Worker integration tests are deferred until a daemon-driven test
    //! harness lands (next slice). Today the worker drives every prompt
    //! through `Client::call_stream_at_session("prompt.stream", ...)`,
    //! which requires a live daemon listening on a Unix socket — heavier
    //! than the previous in-process per-lane Agent cache mocks justified.
    //!
    //! The lane → session mapping is covered end-to-end in
    //! `lane_router::tests::ensure_session_creates_and_caches`. The
    //! command-dispatch shortcut path is covered by
    //! [`worker_routes_slash_help_through_dispatcher`] below — it
    //! exercises the dispatcher branch without touching the daemon.

    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use crate::client::Client;
    use crate::daemon::protocol::{
        Request, Response, StreamFrame, read_frame_bytes, write_frame_bytes, write_response,
    };
    use crate::daemon::server::Server;
    use crate::daemon::state::DaemonState;
    use crate::gateway::commands::CommandDispatcher;
    use crate::gateway::daemon_client::GatewayDaemonClient;
    use crate::gateway::queue::{InboundQueue, OutboundQueue};
    use crate::gateway::render_registry::RenderRegistry;
    use crate::memory::DbPool;
    use crate::platform::{InboundMessage, PlatformCapabilities};
    use std::collections::HashMap;
    use tempfile::tempdir;
    use tokio::net::UnixListener;

    fn fresh_db() -> DbPool {
        crate::memory::in_memory_gateway_pool().expect("in-memory pool")
    }

    /// Smallest possible daemon client for the `/help` test: no daemon
    /// needed because /help bypasses the prompt path.
    fn client_no_daemon() -> GatewayDaemonClient {
        GatewayDaemonClient::with_client_factory(|| {
            Box::pin(async {
                Err(crate::client::ClientError::Protocol(
                    "test client: client factory must not be invoked".into(),
                ))
            })
        })
    }

    #[tokio::test]
    async fn worker_routes_slash_help_through_dispatcher() {
        // YYC-18 PR-2c (preserved across the daemon port): a `/`-prefixed
        // inbound row that maps to a registered command produces an
        // atomic outbound row (no streaming, no edit-in-place). The
        // daemon prompt path is bypassed entirely — turn_id stays None
        // to mark the reply as single-shot.
        let db = fresh_db();
        let inbound = InboundQueue::new(db.clone());
        let outbound = Arc::new(OutboundQueue::new(db.clone(), 5));
        let render_registry = Arc::new(RenderRegistry::new());
        let lane_router = DaemonLaneRouter::new();
        let daemon_client = client_no_daemon();
        let commands = CommandDispatcher::new(&HashMap::new());

        inbound
            .enqueue(InboundMessage {
                platform: "loopback".into(),
                chat_id: "c".into(),
                user_id: "u".into(),
                text: "/help".into(),
                message_id: None,
                reply_to: None,
                attachments: vec![],
            })
            .await
            .unwrap();
        let row = inbound.claim_next().await.unwrap().expect("row");

        process_one(
            row,
            &lane_router,
            &daemon_client,
            &inbound,
            &outbound,
            &render_registry,
            PlatformCapabilities::default(),
            &commands,
        )
        .await
        .unwrap();

        let reply = outbound
            .claim_due(chrono::Utc::now().timestamp())
            .await
            .unwrap()
            .expect("dispatcher reply enqueued");
        assert!(reply.text.starts_with("Available commands:"));
        assert!(reply.text.contains("/help"));
        assert!(
            reply.turn_id.is_none(),
            "command replies are atomic, not streamed"
        );
        assert!(
            inbound.claim_next().await.unwrap().is_none(),
            "inbound row should be done after dispatch, not re-claimable"
        );
    }

    async fn spawn_prompt_daemon(dir: &tempfile::TempDir) -> std::path::PathBuf {
        let sock = dir.path().join("gateway.sock");
        let listener = UnixListener::bind(&sock).expect("bind fake daemon");
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (mut read, mut write) = stream.into_split();
            loop {
                let body = match read_frame_bytes(&mut read).await {
                    Ok(body) => body,
                    Err(_) => break,
                };
                let req: Request = serde_json::from_slice(&body).expect("request");
                match req.method.as_str() {
                    "session.create" => {
                        let resp = Response::ok(req.id, serde_json::json!({ "created": true }));
                        write_response(&mut write, &resp)
                            .await
                            .expect("session response");
                    }
                    "prompt.stream" => {
                        let frame = StreamFrame {
                            version: 1,
                            id: Some(req.id.clone()),
                            stream: "text".into(),
                            data: serde_json::json!({ "chunk": "chunked" }),
                        };
                        let body = serde_json::to_vec(&frame).expect("frame body");
                        write_frame_bytes(&mut write, &body)
                            .await
                            .expect("stream frame");
                        let resp = Response::ok(req.id, serde_json::json!({ "text": "final" }));
                        write_response(&mut write, &resp)
                            .await
                            .expect("prompt response");
                    }
                    other => panic!("unexpected daemon method {other}"),
                }
            }
        });
        sock
    }

    #[tokio::test]
    async fn worker_reuses_gateway_owned_daemon_client_across_rows() {
        let dir = tempdir().unwrap();
        let sock = spawn_prompt_daemon(&dir).await;
        let factory_calls = Arc::new(AtomicUsize::new(0));
        let daemon_client = {
            let sock = sock.clone();
            let calls = Arc::clone(&factory_calls);
            GatewayDaemonClient::with_client_factory(move || {
                let p = sock.clone();
                let calls = Arc::clone(&calls);
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Client::connect_at(&p).await
                })
            })
        };

        let db = fresh_db();
        let inbound = InboundQueue::new(db.clone());
        let outbound = Arc::new(OutboundQueue::new(db.clone(), 5));
        let render_registry = Arc::new(RenderRegistry::new());
        let lane_router = DaemonLaneRouter::new();
        let commands = CommandDispatcher::new(&HashMap::new());

        for text in ["one", "two"] {
            inbound
                .enqueue(InboundMessage {
                    platform: "loopback".into(),
                    chat_id: "c".into(),
                    user_id: "u".into(),
                    text: text.into(),
                    message_id: None,
                    reply_to: None,
                    attachments: vec![],
                })
                .await
                .unwrap();
            let row = inbound.claim_next().await.unwrap().expect("row");
            process_one(
                row,
                &lane_router,
                &daemon_client,
                &inbound,
                &outbound,
                &render_registry,
                PlatformCapabilities::default(),
                &commands,
            )
            .await
            .unwrap();
        }

        assert_eq!(
            factory_calls.load(Ordering::SeqCst),
            1,
            "gateway worker must reuse one daemon client across inbound rows"
        );
    }

    /// End-to-end smoke against a real (tempdir) daemon: an inbound
    /// row routed to `process_one` with no agent provider configured
    /// surfaces `AGENT_BUILD_FAILED` from the daemon; the worker
    /// marks the inbound row failed and the failure path round-trips
    /// cleanly. Confirms the daemon-port wiring without needing a
    /// MockProvider injection seam.
    #[tokio::test]
    #[ignore = "TODO(YYC-266 follow-up): replace with daemon harness that scripts MockProvider once the daemon supports test-mode agent injection"]
    async fn worker_marks_inbound_failed_when_daemon_agent_build_fails() {
        let dir = tempdir().unwrap();
        let sock = dir.path().join("vulcan.sock");
        let state = Arc::new(DaemonState::for_tests_minimal());
        let server = Server::bind(&sock, state.clone()).await.unwrap();
        let server_handle = tokio::spawn(server.run());
        tokio::time::sleep(Duration::from_millis(50)).await;

        let lane_router = DaemonLaneRouter::new();
        let daemon_client = {
            let sock_path = sock.clone();
            GatewayDaemonClient::with_client_factory(move || {
                let p = sock_path.clone();
                Box::pin(async move { Client::connect_at(&p).await })
            })
        };

        let db = fresh_db();
        let inbound = crate::gateway::queue::InboundQueue::with_policy(db.clone(), 1, 60);
        let outbound = Arc::new(OutboundQueue::new(db.clone(), 5));
        let render_registry = Arc::new(RenderRegistry::new());
        let commands = CommandDispatcher::new(&HashMap::new());

        inbound
            .enqueue(InboundMessage {
                platform: "loopback".into(),
                chat_id: "c".into(),
                user_id: "u".into(),
                text: "hello".into(),
                message_id: None,
                reply_to: None,
                attachments: vec![],
            })
            .await
            .unwrap();
        let row = inbound.claim_next().await.unwrap().expect("row");

        let res = process_one(
            row,
            &lane_router,
            &daemon_client,
            &inbound,
            &outbound,
            &render_registry,
            PlatformCapabilities::default(),
            &commands,
        )
        .await;
        assert!(res.is_err(), "process_one must propagate daemon error");

        // Outbound queue should be empty (no reply emitted).
        assert!(
            outbound
                .claim_due(chrono::Utc::now().timestamp())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(inbound.count_dead().await.unwrap(), 1);

        state.signal_shutdown();
        let _ = tokio::time::timeout(Duration::from_secs(2), server_handle).await;
    }
}
