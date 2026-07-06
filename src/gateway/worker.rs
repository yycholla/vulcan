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
use crate::gateway::scheduler_store::SchedulerStore;
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
    scheduler_store: Option<&SchedulerStore>,
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
            let inbound_id = row.id;
            inbound_queue
                .complete_with_outbound(
                    inbound_id,
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
            if let Some(store) = scheduler_store {
                record_scheduler_completion(store, inbound_id).await?;
            }
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
            let inbound_id = row.id;
            inbound_queue
                .complete_with_outbound(
                    inbound_id,
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
            if let Some(store) = scheduler_store {
                record_scheduler_completion(store, inbound_id).await?;
            }
            Ok(())
        }
        Err(e) => {
            let err_str = e.to_string();
            inbound_queue.mark_failed(row.id, &err_str).await?;
            if let Some(store) = scheduler_store {
                record_scheduler_failure(store, row.id, &e).await?;
            }
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
            serde_json::json!({
                "text": input,
                "origin": {
                    "kind": "gateway",
                    "lane": format!("{}:{}", lane.platform, lane.chat_id),
                    "platform": lane.platform,
                    "chat_id": lane.chat_id,
                }
            }),
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

async fn record_scheduler_completion(
    store: &SchedulerStore,
    inbound_id: i64,
) -> anyhow::Result<()> {
    let finished_at = chrono::Utc::now().timestamp();
    store
        .record_completed_by_inbound(inbound_id, finished_at)
        .await
        .map_err(|e| anyhow::anyhow!("scheduler completion persistence: {e}"))
}

async fn record_scheduler_failure(
    store: &SchedulerStore,
    inbound_id: i64,
    error: &anyhow::Error,
) -> anyhow::Result<()> {
    let finished_at = chrono::Utc::now().timestamp();
    let message = scheduler_failure_message(error);
    store
        .record_run_failed_by_inbound(inbound_id, finished_at, &message)
        .await
        .map_err(|e| anyhow::anyhow!("scheduler failure persistence: {e}"))
}

fn scheduler_failure_message(error: &anyhow::Error) -> String {
    let text = error.to_string();
    if let Some(message) = text.strip_prefix("daemon prompt.stream error [")
        && let Some((_, message)) = message.split_once("]: ")
    {
        return message.to_string();
    }
    text
}
