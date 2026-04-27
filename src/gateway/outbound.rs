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
    /// the loop on drop (mirrors `EvictorHandle` from agent_map.rs).
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
        let msg = OutboundMessage {
            platform: row.platform,
            chat_id: row.chat_id,
            text: row.text,
            attachments: row.attachments,
            reply_to: row.reply_to,
            edit_target: row.edit_target,
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
                    // Turn id isn't stored on the row in PR-2a — use chat_id
                    // as a stand-in. PR-2b adds a turn_id column when it
                    // wires the worker streaming path.
                    let key = RenderKey {
                        platform: msg.platform.clone(),
                        chat_id: msg.chat_id.clone(),
                        turn_id: msg.chat_id.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::gateway::loopback::LoopbackPlatform;
    use crate::gateway::queue::OutboundQueue;
    use crate::gateway::registry::PlatformRegistry;
    use crate::memory::DbPool;
    use crate::platform::OutboundMessage;

    fn fresh_db() -> DbPool {
        crate::memory::in_memory_gateway_pool().expect("in-memory pool")
    }

    fn out_msg(text: &str) -> OutboundMessage {
        OutboundMessage {
            platform: "loopback".into(),
            chat_id: "c".into(),
            text: text.into(),
            attachments: vec![],
            reply_to: None,
            edit_target: None,
        }
    }

    #[tokio::test]
    async fn dispatcher_delivers_pending_outbound() {
        let q = Arc::new(OutboundQueue::new(fresh_db(), 5));
        let lp = Arc::new(LoopbackPlatform::default());
        let mut reg = PlatformRegistry::new();
        reg.register("loopback", lp.clone());
        let reg = Arc::new(reg);
        let render_reg = Arc::new(RenderRegistry::new());
        let dispatcher = OutboundDispatcher::new(q.clone(), reg, render_reg)
            .with_poll_interval(Duration::from_millis(20))
            .spawn();

        q.enqueue(out_msg("hello")).await.unwrap();

        // Poll up to 1s for delivery.
        let mut delivered = 0;
        for _ in 0..50 {
            if !lp.recorded().await.is_empty() {
                delivered = lp.recorded().await.len();
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(delivered, 1);
        drop(dispatcher);
    }

    use std::sync::atomic::{AtomicUsize, Ordering};

    struct EditTrackingPlatform {
        send_calls: Arc<AtomicUsize>,
        edit_calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl crate::platform::Platform for EditTrackingPlatform {
        fn name(&self) -> &str {
            "edit-tracker"
        }
        async fn start(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn send(
            &self,
            _msg: &OutboundMessage,
        ) -> anyhow::Result<crate::platform::SentMessage> {
            self.send_calls.fetch_add(1, Ordering::SeqCst);
            Ok(crate::platform::SentMessage {
                message_id: "tracked-1".into(),
            })
        }
        async fn edit(&self, _c: &str, _m: &str, _t: &str) -> anyhow::Result<()> {
            self.edit_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn recv(&self) -> anyhow::Result<crate::platform::InboundMessage> {
            anyhow::bail!("no recv")
        }
    }

    #[tokio::test]
    async fn dispatcher_routes_to_edit_when_edit_target_set() {
        let queue = Arc::new(OutboundQueue::new(fresh_db(), 5));
        let render_reg = Arc::new(RenderRegistry::new());
        let send_calls = Arc::new(AtomicUsize::new(0));
        let edit_calls = Arc::new(AtomicUsize::new(0));
        let plat = Arc::new(EditTrackingPlatform {
            send_calls: send_calls.clone(),
            edit_calls: edit_calls.clone(),
        });
        let mut reg = PlatformRegistry::new();
        reg.register("edit-tracker", plat);
        let registry = Arc::new(reg);

        queue
            .enqueue(OutboundMessage {
                platform: "edit-tracker".into(),
                chat_id: "c".into(),
                text: "edit me".into(),
                attachments: vec![],
                reply_to: None,
                edit_target: Some("anchor-1".into()),
            })
            .await
            .unwrap();
        drain_due(&queue, &registry, &render_reg).await.unwrap();
        assert_eq!(send_calls.load(Ordering::SeqCst), 0);
        assert_eq!(edit_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dispatcher_captures_send_anchor_into_render_registry() {
        let queue = Arc::new(OutboundQueue::new(fresh_db(), 5));
        let render_reg = Arc::new(RenderRegistry::new());
        let plat = Arc::new(LoopbackPlatform::default());
        let mut reg = PlatformRegistry::new();
        reg.register("loopback", plat);
        let registry = Arc::new(reg);

        queue
            .enqueue(OutboundMessage {
                platform: "loopback".into(),
                chat_id: "c".into(),
                text: "first".into(),
                attachments: vec![],
                reply_to: None,
                edit_target: None,
            })
            .await
            .unwrap();
        drain_due(&queue, &registry, &render_reg).await.unwrap();
        let anchor = render_reg.anchor(&RenderKey {
            platform: "loopback".into(),
            chat_id: "c".into(),
            turn_id: "c".into(),
        });
        assert!(anchor.is_some(), "first send should populate the registry");
    }

    #[tokio::test]
    async fn failed_send_retries_after_backoff() {
        let db = fresh_db();
        let q = Arc::new(OutboundQueue::new(db.clone(), 5));
        let lp = Arc::new(LoopbackPlatform::failing_first(2));
        let mut reg = PlatformRegistry::new();
        reg.register("loopback", lp.clone());
        let reg = Arc::new(reg);

        let id = q.enqueue(out_msg("payload")).await.unwrap();

        let render_reg = Arc::new(RenderRegistry::new());
        let dispatcher = OutboundDispatcher::new(q.clone(), reg, render_reg)
            .with_poll_interval(Duration::from_millis(20))
            .spawn();

        // After each failure mark_failed bumps next_attempt_at by [5s, 30s, ...].
        // Without rewinding we'd wait 35s of real time. Instead, between ticks
        // we rewind next_attempt_at to "now" so the dispatcher reclaims the
        // row on its next tick — avoids waiting the real 5s+30s backoff.
        let mut ok = false;
        for _ in 0..40 {
            {
                let conn = db.get().expect("checkout");
                let now = chrono::Utc::now().timestamp();
                let _ = conn.execute(
                    "UPDATE outbound_queue SET next_attempt_at = ?1 WHERE id = ?2 AND state = 'pending'",
                    rusqlite::params![now, id],
                );
            }
            if lp.recorded().await.len() == 1 {
                ok = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(ok, "expected eventual delivery after 2 failures");
        drop(dispatcher);
    }
}
