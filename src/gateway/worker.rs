//! Lane worker compatibility wrapper.
//!
//! The lane router (`mod.rs`'s inbound dispatcher) hands one inbound row
//! at a time to `process_one`. Inbound Turn delivery lives in
//! [`crate::gateway::turn_delivery`]; this module keeps the worker-facing
//! signature stable for the dispatcher.

use std::sync::Arc;

use crate::gateway::commands::CommandDispatcher;
use crate::gateway::daemon_client::GatewayDaemonClient;
use crate::gateway::lane_router::DaemonLaneRouter;
use crate::gateway::queue::{InboundQueue, InboundRow, OutboundQueue};
use crate::gateway::render_registry::RenderRegistry;
use crate::gateway::turn_delivery::TurnDelivery;
use crate::platform::PlatformCapabilities;

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
    outbound_queue: &Arc<OutboundQueue>,
    render_registry: &Arc<RenderRegistry>,
    platform_caps: PlatformCapabilities,
    commands: &CommandDispatcher,
) -> anyhow::Result<()> {
    TurnDelivery::new(
        lane_router,
        daemon_client,
        inbound_queue,
        outbound_queue,
        render_registry,
        platform_caps,
        commands,
    )
    .deliver(row)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::client::Client;
    use crate::daemon::protocol::{
        ProtocolError, Request, Response, StreamFrame, read_frame_bytes, write_frame_bytes,
        write_response,
    };
    use crate::gateway::commands::CommandDispatcher;
    use crate::gateway::daemon_client::GatewayDaemonClient;
    use crate::gateway::lane_router::DaemonLaneRouter;
    use crate::gateway::queue::{InboundQueue, OutboundQueue};
    use crate::gateway::render_registry::RenderRegistry;
    use crate::gateway::turn_delivery::{DeliveryOutcome, TurnDelivery};
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

    fn render_registry() -> Arc<RenderRegistry> {
        Arc::new(RenderRegistry::new())
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
        let renders = render_registry();
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

        let outcome = TurnDelivery::new(
            &lane_router,
            &daemon_client,
            &inbound,
            &outbound,
            &renders,
            PlatformCapabilities::default(),
            &commands,
        )
        .deliver(row)
        .await
        .unwrap();
        assert_eq!(outcome, DeliveryOutcome::CommandShortcut);

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

    #[derive(Clone, Copy)]
    enum PromptScript {
        Success,
        SuccessWithToolFrame,
        Error,
    }

    async fn spawn_prompt_daemon(
        dir: &tempfile::TempDir,
        script: PromptScript,
    ) -> std::path::PathBuf {
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
                    "daemon.handshake" => {
                        let resp = Response::ok(
                            req.id,
                            serde_json::json!({
                                "ok": true,
                                "frontend_capabilities": req.frontend_capabilities,
                            }),
                        );
                        write_response(&mut write, &resp)
                            .await
                            .expect("handshake response");
                    }
                    "session.create" => {
                        let resp = Response::ok(req.id, serde_json::json!({ "created": true }));
                        write_response(&mut write, &resp)
                            .await
                            .expect("session response");
                    }
                    "prompt.stream" => match script {
                        PromptScript::Success | PromptScript::SuccessWithToolFrame => {
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
                            if matches!(script, PromptScript::SuccessWithToolFrame) {
                                let frame = StreamFrame {
                                    version: 1,
                                    id: Some(req.id.clone()),
                                    stream: "tool_call_start".into(),
                                    data: serde_json::json!({
                                        "tool_id": "tool-1",
                                        "name": "noop",
                                        "args_summary": "arg"
                                    }),
                                };
                                let body = serde_json::to_vec(&frame).expect("frame body");
                                write_frame_bytes(&mut write, &body)
                                    .await
                                    .expect("tool stream frame");
                            }
                            let resp = Response::ok(req.id, serde_json::json!({ "text": "final" }));
                            write_response(&mut write, &resp)
                                .await
                                .expect("prompt response");
                        }
                        PromptScript::Error => {
                            let resp = Response::error(
                                req.id,
                                ProtocolError {
                                    code: "TEST_DAEMON_FAILURE".into(),
                                    message: "scripted prompt failure".into(),
                                    retryable: false,
                                },
                            );
                            write_response(&mut write, &resp)
                                .await
                                .expect("prompt error response");
                        }
                    },
                    other => panic!("unexpected daemon method {other}"),
                }
            }
        });
        sock
    }

    #[tokio::test]
    async fn worker_reuses_gateway_owned_daemon_client_across_rows() {
        let dir = tempdir().unwrap();
        let sock = spawn_prompt_daemon(&dir, PromptScript::Success).await;
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
        let renders = render_registry();
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
            let outcome = TurnDelivery::new(
                &lane_router,
                &daemon_client,
                &inbound,
                &outbound,
                &renders,
                PlatformCapabilities::default(),
                &commands,
            )
            .deliver(row)
            .await
            .unwrap();
            assert_eq!(outcome, DeliveryOutcome::DaemonPrompt);

            let reply = outbound
                .claim_due(chrono::Utc::now().timestamp())
                .await
                .unwrap()
                .expect("prompt reply enqueued");
            assert_eq!(reply.text, "final");
            assert_eq!(reply.platform, "loopback");
            assert_eq!(reply.chat_id, "c");
        }

        assert_eq!(
            factory_calls.load(Ordering::SeqCst),
            1,
            "gateway worker must reuse one daemon client across inbound rows"
        );
    }

    #[tokio::test]
    async fn worker_keeps_buffered_reply_when_non_text_frame_arrives_without_edit_support() {
        let dir = tempdir().unwrap();
        let sock = spawn_prompt_daemon(&dir, PromptScript::SuccessWithToolFrame).await;
        let daemon_client = {
            let sock = sock.clone();
            GatewayDaemonClient::with_client_factory(move || {
                let p = sock.clone();
                Box::pin(async move { Client::connect_at(&p).await })
            })
        };

        let db = fresh_db();
        let inbound = InboundQueue::new(db.clone());
        let outbound = Arc::new(OutboundQueue::new(db.clone(), 5));
        let renders = render_registry();
        let lane_router = DaemonLaneRouter::new();
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
        let outcome = TurnDelivery::new(
            &lane_router,
            &daemon_client,
            &inbound,
            &outbound,
            &renders,
            PlatformCapabilities::default(),
            &commands,
        )
        .deliver(row)
        .await
        .unwrap();
        assert_eq!(outcome, DeliveryOutcome::DaemonPrompt);

        let reply = outbound
            .claim_due(chrono::Utc::now().timestamp())
            .await
            .unwrap()
            .expect("prompt reply enqueued");
        assert_eq!(reply.text, "final");
        assert!(reply.edit_target.is_none());
        assert!(reply.turn_id.is_none());
        assert!(
            outbound
                .claim_due(chrono::Utc::now().timestamp())
                .await
                .unwrap()
                .is_none(),
            "no-edit platforms still emit one buffered reply"
        );
    }

    #[tokio::test]
    async fn worker_marks_inbound_failed_when_daemon_prompt_fails() {
        let dir = tempdir().unwrap();
        let sock = spawn_prompt_daemon(&dir, PromptScript::Error).await;

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
        let renders = render_registry();
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

        let res = TurnDelivery::new(
            &lane_router,
            &daemon_client,
            &inbound,
            &outbound,
            &renders,
            PlatformCapabilities::default(),
            &commands,
        )
        .deliver(row)
        .await;
        assert!(res.is_err(), "delivery must propagate daemon error");

        // Outbound queue should be empty (no reply emitted).
        assert!(
            outbound
                .claim_due(chrono::Utc::now().timestamp())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(inbound.count_dead().await.unwrap(), 1);
    }
}
