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
use crate::platform::OutboundMessage;

pub struct OutboundDispatcher {
    queue: Arc<OutboundQueue>,
    registry: Arc<PlatformRegistry>,
    poll_interval: Duration,
}

impl OutboundDispatcher {
    pub fn new(queue: Arc<OutboundQueue>, registry: Arc<PlatformRegistry>) -> Self {
        Self {
            queue,
            registry,
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
            poll_interval,
        } = self;
        let handle = tokio::spawn(dispatch_loop(queue, registry, poll_interval));
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
    poll_interval: Duration,
) {
    let mut ticker = tokio::time::interval(poll_interval);
    // Skip missed ticks rather than bursting them after a long pause (laptop
    // sleep, GC stall) — drain_due is idempotent so the next tick handles
    // whatever the burst would have done in one pass.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        if let Err(e) = drain_due(&queue, &registry).await {
            tracing::error!(target: "gateway::outbound", error = %e, "drain_due errored");
        }
    }
}

async fn drain_due(queue: &OutboundQueue, registry: &PlatformRegistry) -> anyhow::Result<()> {
    loop {
        let now = chrono::Utc::now().timestamp();
        let Some(row) = queue.claim_due(now).await? else {
            return Ok(());
        };
        let id = row.id;
        let msg = OutboundMessage {
            platform: row.platform,
            chat_id: row.chat_id,
            text: row.text,
            attachments: row.attachments,
        };
        match registry.send(&msg).await {
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
        }
    }

    #[tokio::test]
    async fn dispatcher_delivers_pending_outbound() {
        let q = Arc::new(OutboundQueue::new(fresh_db(), 5));
        let lp = Arc::new(LoopbackPlatform::default());
        let mut reg = PlatformRegistry::new();
        reg.register("loopback", lp.clone());
        let reg = Arc::new(reg);
        let dispatcher = OutboundDispatcher::new(q.clone(), reg)
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

    #[tokio::test]
    async fn failed_send_retries_after_backoff() {
        let db = fresh_db();
        let q = Arc::new(OutboundQueue::new(db.clone(), 5));
        let lp = Arc::new(LoopbackPlatform::failing_first(2));
        let mut reg = PlatformRegistry::new();
        reg.register("loopback", lp.clone());
        let reg = Arc::new(reg);

        let id = q.enqueue(out_msg("payload")).await.unwrap();

        let dispatcher = OutboundDispatcher::new(q.clone(), reg)
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
