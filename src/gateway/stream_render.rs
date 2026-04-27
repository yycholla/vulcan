//! StreamRenderer — turns a stream of `StreamEvent`s into OutboundMessages.
//!
//! First chunk: emits an `OutboundMessage` with `edit_target = None`.
//! When the OutboundDispatcher delivers it, it captures the returned
//! `SentMessage::message_id` into RenderRegistry under
//! (platform, chat_id, turn_id). Subsequent chunks read that anchor
//! and emit `OutboundMessage { edit_target: Some(anchor), .. }` so
//! the dispatcher routes to `Platform::edit`.
//!
//! Throttle: emits at most one OutboundMessage per
//! `edit_min_interval_ms` (from the platform's capabilities). Tool
//! call boundaries (`StreamEvent::ToolCallStart` / `ToolCallEnd`)
//! and `StreamEvent::Done` / `Error` flush immediately.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::gateway::queue::OutboundQueue;
use crate::gateway::render_registry::{RenderKey, RenderRegistry};
use crate::platform::OutboundMessage;
use crate::provider::StreamEvent;

pub struct StreamRenderer {
    key: RenderKey,
    interval: Duration,
    buffer: String,
    last_emit: Instant,
    outbound: Arc<OutboundQueue>,
    registry: Arc<RenderRegistry>,
    /// "Have we enqueued at least one outbound row for this turn?"
    /// First chunk: false → emit with `edit_target = None`. Subsequent
    /// chunks: true → read RenderRegistry for the anchor.
    ///
    /// Race window — PR-2b TODO: between `enqueued_first = true` (here)
    /// and the dispatcher writing the anchor (after Platform::send
    /// returns), a second chunk that fires inside the throttle interval
    /// reads `None` and emits another no-anchor send, producing a
    /// duplicate first message on the user's screen. The 1s Telegram
    /// edit-floor + the dispatcher's 250ms poll make this a narrow
    /// window in practice; PR-2b should tighten it by either
    ///   (a) blocking subsequent enqueues until the anchor lands, or
    ///   (b) capturing the anchor synchronously inside the renderer
    ///       (skip the dispatcher round-trip for first send).
    enqueued_first: bool,
}

impl StreamRenderer {
    pub fn new(
        key: RenderKey,
        edit_min_interval_ms: u64,
        outbound: Arc<OutboundQueue>,
        registry: Arc<RenderRegistry>,
    ) -> Self {
        Self {
            key,
            interval: Duration::from_millis(edit_min_interval_ms),
            buffer: String::new(),
            last_emit: Instant::now() - Duration::from_secs(3600),
            outbound,
            registry,
            enqueued_first: false,
        }
    }

    pub async fn handle(&mut self, ev: StreamEvent) -> anyhow::Result<()> {
        match ev {
            StreamEvent::Text(chunk) | StreamEvent::Reasoning(chunk) => {
                self.buffer.push_str(&chunk);
                if self.last_emit.elapsed() >= self.interval {
                    self.flush().await?;
                }
            }
            StreamEvent::ToolCallStart { .. } | StreamEvent::ToolCallEnd { .. } => {
                self.flush().await?;
            }
            // PR-2b TODO: surface ChatResponse.finish_reason / usage on
            // the final flush so the chat surface can render a "✓ done"
            // footer with token counts.
            StreamEvent::Done(_) | StreamEvent::Error(_) => {
                self.flush().await?;
                // Forget the anchor — turn is over.
                self.registry.forget(&self.key);
            }
        }
        Ok(())
    }

    async fn flush(&mut self) -> anyhow::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let edit_target = if self.enqueued_first {
            self.registry.anchor(&self.key)
        } else {
            None
        };
        let msg = OutboundMessage {
            platform: self.key.platform.clone(),
            chat_id: self.key.chat_id.clone(),
            text: self.buffer.clone(),
            attachments: Vec::new(),
            reply_to: None,
            edit_target,
        };
        self.outbound.enqueue(msg).await?;
        self.last_emit = Instant::now();
        self.enqueued_first = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::in_memory_gateway_pool;

    fn key() -> RenderKey {
        RenderKey {
            platform: "loopback".into(),
            chat_id: "c".into(),
            turn_id: "t1".into(),
        }
    }

    fn fresh_outbound() -> Arc<OutboundQueue> {
        Arc::new(OutboundQueue::new(in_memory_gateway_pool().unwrap(), 5))
    }

    #[tokio::test]
    async fn first_text_chunk_enqueues_send_with_no_edit_target() {
        let outbound = fresh_outbound();
        let registry = Arc::new(RenderRegistry::new());
        let mut r = StreamRenderer::new(key(), 0, outbound.clone(), registry);
        r.handle(StreamEvent::Text("hello".into())).await.unwrap();
        let row = outbound
            .claim_due(chrono::Utc::now().timestamp())
            .await
            .unwrap()
            .expect("row");
        assert_eq!(row.text, "hello");
        assert!(row.edit_target.is_none());
    }

    #[tokio::test]
    async fn subsequent_chunk_uses_anchor_from_registry() {
        let outbound = fresh_outbound();
        let registry = Arc::new(RenderRegistry::new());
        let mut r = StreamRenderer::new(key(), 0, outbound.clone(), registry.clone());
        r.handle(StreamEvent::Text("first ".into())).await.unwrap();
        // Simulate dispatcher capturing the anchor.
        registry.set_anchor(key(), "msg-7".into());
        r.handle(StreamEvent::Text("second".into())).await.unwrap();
        let _row1 = outbound
            .claim_due(chrono::Utc::now().timestamp())
            .await
            .unwrap()
            .unwrap();
        let row2 = outbound
            .claim_due(chrono::Utc::now().timestamp())
            .await
            .unwrap()
            .expect("second row");
        assert_eq!(row2.text, "first second");
        assert_eq!(row2.edit_target.as_deref(), Some("msg-7"));
    }

    #[tokio::test]
    async fn done_flushes_and_forgets_anchor() {
        let outbound = fresh_outbound();
        let registry = Arc::new(RenderRegistry::new());
        let mut r = StreamRenderer::new(key(), 0, outbound.clone(), registry.clone());
        r.handle(StreamEvent::Text("partial".into())).await.unwrap();
        registry.set_anchor(key(), "msg-1".into());
        r.handle(StreamEvent::Done(crate::provider::ChatResponse {
            content: None,
            tool_calls: None,
            usage: None,
            finish_reason: None,
            reasoning_content: None,
        }))
        .await
        .unwrap();
        assert!(
            registry.anchor(&key()).is_none(),
            "Done should forget the anchor"
        );
    }

    #[tokio::test]
    async fn throttle_holds_chunks_within_interval() {
        let outbound = fresh_outbound();
        let registry = Arc::new(RenderRegistry::new());
        // 1 second floor — first chunk emits, second chunk shouldn't.
        let mut r = StreamRenderer::new(key(), 1000, outbound.clone(), registry);
        r.handle(StreamEvent::Text("a".into())).await.unwrap();
        r.handle(StreamEvent::Text("b".into())).await.unwrap();
        // Only one row should be claimable — the second chunk was buffered.
        let row1 = outbound
            .claim_due(chrono::Utc::now().timestamp())
            .await
            .unwrap()
            .expect("row1");
        assert_eq!(row1.text, "a");
        let row2 = outbound
            .claim_due(chrono::Utc::now().timestamp())
            .await
            .unwrap();
        assert!(row2.is_none(), "second chunk should be held by throttle");
    }
}
