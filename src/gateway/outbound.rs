//! Outbound dispatcher loop.
//!
//! Polls `OutboundQueue::claim_due` on a tick, hands each row to
//! `PlatformRegistry::send`, and on failure calls `OutboundQueue::mark_failed`
//! so the queue's existing backoff schedule (Task 6) reschedules the row.
//!
//! The queue is durable, so on startup the loop also drains anything that
//! survived a restart.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::gateway::queue::OutboundQueue;
use crate::gateway::registry::PlatformRegistry;
use crate::gateway::render_registry::{RenderKey, RenderRegistry};
use crate::platform::OutboundMessage;

pub struct OutboundDispatcher {
    queue: Arc<OutboundQueue>,
    registry: Arc<PlatformRegistry>,
    render_registry: Arc<RenderRegistry>,
    poll_interval: Duration,
}

impl OutboundDispatcher {
    pub fn new(
        queue: Arc<OutboundQueue>,
        registry: Arc<PlatformRegistry>,
        render_registry: Arc<RenderRegistry>,
    ) -> Self {
        Self {
            queue,
            registry,
            render_registry,
            poll_interval: Duration::from_millis(250),
        }
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Spawn the polling loop. The returned `OutboundDispatcherHandle` aborts
    /// the loop on drop (drop-aborted, like other gateway side-task handles).
    pub fn spawn(self) -> OutboundDispatcherHandle {
        let Self {
            queue,
            registry,
            render_registry,
            poll_interval,
        } = self;
        let handle = tokio::spawn(dispatch_loop(
            queue,
            registry,
            render_registry,
            poll_interval,
        ));
        OutboundDispatcherHandle { handle }
    }
}

pub struct OutboundDispatcherHandle {
    handle: JoinHandle<()>,
}

impl OutboundDispatcherHandle {
    pub fn abort(&self) {
        self.handle.abort();
    }
}

impl Drop for OutboundDispatcherHandle {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn dispatch_loop(
    queue: Arc<OutboundQueue>,
    registry: Arc<PlatformRegistry>,
    render_registry: Arc<RenderRegistry>,
    poll_interval: Duration,
) {
    // YYC-146: lifecycle log so operators can confirm the dispatcher
    // is running without grepping for delivery events.
    tracing::info!(
        target: "gateway::outbound",
        poll_ms = poll_interval.as_millis() as u64,
        "outbound dispatcher started",
    );
    let mut ticker = tokio::time::interval(poll_interval);
    // Skip missed ticks rather than bursting them after a long pause (laptop
    // sleep, GC stall) — drain_due is idempotent so the next tick handles
    // whatever the burst would have done in one pass.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        if let Err(e) = drain_due(&queue, &registry, &render_registry).await {
            tracing::error!(target: "gateway::outbound", error = %e, "drain_due errored");
        }
    }
}

async fn drain_due(
    queue: &OutboundQueue,
    registry: &PlatformRegistry,
    render_registry: &RenderRegistry,
) -> anyhow::Result<()> {
    loop {
        let now = chrono::Utc::now().timestamp();
        let Some(row) = queue.claim_due(now).await? else {
            return Ok(());
        };
        let id = row.id;
        let edit_target = row.edit_target.clone();
        let turn_id = row.turn_id.clone();
        let msg = OutboundMessage {
            platform: row.platform,
            chat_id: row.chat_id,
            text: row.text,
            attachments: row.attachments,
            reply_to: row.reply_to,
            edit_target: row.edit_target,
            turn_id: row.turn_id,
        };
        let result = if let Some(anchor) = edit_target {
            // Route to edit; ignore SentMessage shape (edit returns ()).
            // PR-2b TODO: on persistent edit failure (anchor stale,
            // message deleted, message-too-old), forget the registry
            // anchor and fall back to a fresh send rather than retry
            // the same edit forever. Today, mark_failed reschedules
            // and the next attempt re-edits the same anchor.
            let plat = registry
                .get(&msg.platform)
                .ok_or_else(|| anyhow::anyhow!("unknown platform: {}", msg.platform))?;
            plat.edit(&msg.chat_id, &anchor, &msg.text).await
        } else {
            // First send: capture the SentMessage id into RenderRegistry
            // under the lane key so the next chunk can target it.
            let plat = registry
                .get(&msg.platform)
                .ok_or_else(|| anyhow::anyhow!("unknown platform: {}", msg.platform))?;
            match plat.send(&msg).await {
                Ok(sent) => {
                    // Build the RenderKey from (platform, chat_id, turn_id).
                    // turn_id is populated by StreamRenderer for streaming
                    // rows; non-streaming rows (CommandDispatcher replies,
                    // /v1/inbound webhooks) fall back to chat_id so the
                    // registry key still scopes per-lane.
                    let key = RenderKey {
                        platform: msg.platform.clone(),
                        chat_id: msg.chat_id.clone(),
                        turn_id: turn_id.clone().unwrap_or_else(|| msg.chat_id.clone()),
                    };
                    // PR-2b TODO: anchor write happens before mark_done.
                    // If the subsequent mark_done fails (DB pool / disk),
                    // recover_sending will re-deliver the row → second
                    // Platform::send → second visible message, but the
                    // registry still holds the *first* anchor. The next
                    // edit then targets a message whose body diverges
                    // from what the registry expects. Move the
                    // set_anchor call into a transaction with mark_done
                    // (or capture-after-mark-done) when worker streaming
                    // lands.
                    render_registry.set_anchor(key, sent.message_id);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        };
        match result {
            Ok(()) => queue.mark_done(id).await?,
            Err(e) => queue.mark_failed(id, &e.to_string()).await?,
        }
    }
}
