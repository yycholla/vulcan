//! Lane worker — pulls a claimed inbound row, drives the per-lane Agent in
//! streaming mode, and pumps each `StreamEvent` through `StreamRenderer` so
//! it lands on the outbound queue as a series of edit-in-place updates.
//!
//! The lane router (`mod.rs`'s inbound dispatcher) hands one inbound row at a
//! time to `process_one`. We deliberately catch panics from
//! `Agent::run_prompt_stream` so a single wedged Agent doesn't kill the whole
//! lane worker task — the row gets marked `failed` and the worker loop
//! survives to claim the next row.

use std::sync::Arc;

use crate::gateway::agent_map::AgentMap;
use crate::gateway::lane::LaneKey;
use crate::gateway::queue::{InboundQueue, InboundRow, OutboundQueue};
use crate::gateway::render_registry::{RenderKey, RenderRegistry};
use crate::gateway::stream_render::StreamRenderer;
use crate::platform::PlatformCapabilities;

use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;
use uuid::Uuid;

/// Drive one inbound row through its Agent and pump streamed output to the
/// outbound queue via `StreamRenderer`.
///
/// Steps:
/// 1. Mint a fresh per-turn UUID and build the `RenderKey` for this turn.
/// 2. Look up or spawn the Agent for the row's lane.
/// 3. Run the prompt under `run_prompt_stream`, with a consumer task draining
///    `StreamEvent`s into the renderer.
/// 4. Catch panics so the lane worker survives a wedged Agent; on panic, evict
///    the lane so the next request rebuilds fresh state.
/// 5. On success: mark the inbound row `done` (the renderer has already
///    enqueued the streamed output, including the final flush triggered by
///    `StreamEvent::Done`).
/// 6. On failure (Err or panic): mark the inbound row `failed` and bubble
///    the error up to the lane worker for logging.
pub async fn process_one(
    row: InboundRow,
    agent_map: &AgentMap,
    inbound_queue: &InboundQueue,
    outbound_queue: &Arc<OutboundQueue>,
    render_registry: &Arc<RenderRegistry>,
    platform_caps: PlatformCapabilities,
) -> anyhow::Result<()> {
    let lane = LaneKey {
        platform: row.platform.clone(),
        chat_id: row.chat_id.clone(),
    };

    let turn_id = Uuid::new_v4().to_string();
    let render_key = RenderKey {
        platform: row.platform.clone(),
        chat_id: row.chat_id.clone(),
        turn_id: turn_id.clone(),
    };

    // Build the renderer once; it'll be moved into the consumer task.
    let mut renderer = StreamRenderer::new(
        render_key,
        platform_caps.edit_min_interval_ms,
        outbound_queue.clone(),
        render_registry.clone(),
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::provider::StreamEvent>(
        crate::provider::STREAM_CHANNEL_CAPACITY,
    );

    // Consumer task: pump StreamEvents through the renderer. Lives until the
    // sender side (held by run_prompt_stream) is dropped.
    //
    // Returns `Err(anyhow::Error)` on the first `renderer.handle` failure
    // (e.g., outbound enqueue failed) so the worker can mark the inbound
    // row failed instead of silently swallowing a broken render. A panic
    // inside this task is caught by `consumer.await` returning `JoinError`
    // — also handled below as a turn-level failure.
    let consumer = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            renderer.handle(ev).await?;
        }
        Ok::<_, anyhow::Error>(())
    });

    // AssertUnwindSafe: we don't read Agent state after a panic — we just
    // mark the inbound row failed and drop our handle. The Agent's own state
    // may be inconsistent, but the next get_or_spawn after eviction will
    // build a fresh one. This is the same trade-off catch_unwind always
    // makes; the alternative (poisoning the lane forever) is worse.
    let unwind = AssertUnwindSafe(async {
        let agent = agent_map.get_or_spawn(&lane).await?;
        let mut agent = agent.lock().await;
        agent.run_prompt_stream(&row.text, tx).await
    })
    .catch_unwind()
    .await;

    // The sender was moved into run_prompt_stream; closing it lets the
    // consumer drain to completion before we mark the inbound row done.
    let consumer_outcome = consumer.await;

    let panicked = unwind.is_err();
    let mut result: anyhow::Result<String> = unwind.unwrap_or_else(|payload| {
        let msg = payload
            .downcast_ref::<&'static str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        Err(anyhow::anyhow!(
            "agent panicked while running prompt: {msg}"
        ))
    });

    // Surface renderer failures the same as agent failures. Two ways the
    // consumer can fail:
    //   1. JoinError — task panicked. Treat as a turn failure.
    //   2. Inner `Err` from `renderer.handle` — outbound enqueue failed,
    //      DB pool error, etc. The user will see no message; mark the
    //      inbound row failed so retry / DLQ semantics kick in.
    // Don't override an existing `Err` from the agent path — the first
    // error wins.
    if result.is_ok() {
        match consumer_outcome {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                result = Err(e.context("stream renderer failed"));
            }
            Err(join_err) => {
                result = Err(anyhow::anyhow!("stream renderer task panicked: {join_err}"));
            }
        }
    } else if let Err(join_err) = consumer_outcome {
        // Agent already failed; just log the consumer panic so it's not
        // silently lost.
        tracing::warn!(
            target: "gateway::worker",
            error = %join_err,
            "stream renderer task panicked (agent path also failed)",
        );
    }

    // YYC-133: if the future panicked, the Agent's internal state is
    // suspect — message buffer mid-write, tool registry mid-mutation,
    // hooks holding partial state. Force-evict the lane entry so the
    // *next* inbound row builds a fresh Agent rather than locking
    // against a deadlocked / corrupted one.
    if panicked {
        let removed = agent_map.evict(&lane).await;
        tracing::warn!(
            lane = %row.platform,
            chat_id = %row.chat_id,
            removed,
            "evicted lane agent after panic; next request will build fresh state",
        );
    }

    match result {
        Ok(_final_text) => {
            // The renderer has already enqueued the streamed output
            // (including the final flush triggered by StreamEvent::Done).
            // Just mark the inbound row done.
            inbound_queue.mark_done(row.id).await?;
            Ok(())
        }
        Err(e) => {
            let err_str = e.to_string();
            inbound_queue.mark_failed(row.id, &err_str).await?;
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::Result;
    use async_trait::async_trait;
    use tokio_util::sync::CancellationToken;

    use crate::agent::Agent;
    use crate::config::Config;
    use crate::gateway::agent_map::{AgentBuilder, AgentMap};
    use crate::gateway::queue::{InboundQueue, OutboundQueue};
    use crate::gateway::render_registry::RenderRegistry;
    use crate::hooks::HookRegistry;
    use crate::memory::DbPool;
    use crate::platform::{InboundMessage, PlatformCapabilities};
    use crate::provider::mock::MockProvider;
    use crate::provider::{ChatResponse, LLMProvider, Message, StreamEvent, ToolDefinition};
    use crate::skills::SkillRegistry;
    use crate::tools::ToolRegistry;

    fn fresh_db() -> DbPool {
        crate::memory::in_memory_gateway_pool().expect("in-memory pool")
    }

    fn empty_skills() -> Arc<SkillRegistry> {
        Arc::new(SkillRegistry::new(&std::path::PathBuf::from(
            "/tmp/vulcan-test-skills-nonexistent",
        )))
    }

    /// Wraps an Arc<MockProvider> so multiple Agents can share the same
    /// scripted queue. Mirrors `agent::tests::agent_with_mock`.
    struct ProviderHandle(Arc<MockProvider>);
    #[async_trait]
    impl LLMProvider for ProviderHandle {
        async fn chat(
            &self,
            m: &[Message],
            t: &[ToolDefinition],
            c: CancellationToken,
        ) -> Result<ChatResponse> {
            self.0.chat(m, t, c).await
        }
        async fn chat_stream(
            &self,
            m: &[Message],
            t: &[ToolDefinition],
            tx: tokio::sync::mpsc::Sender<StreamEvent>,
            c: CancellationToken,
        ) -> Result<()> {
            self.0.chat_stream(m, t, tx, c).await
        }
        fn max_context(&self) -> usize {
            self.0.max_context()
        }
    }

    /// Provider that panics on every call. Used to verify `process_one`
    /// catches Agent panics and marks the inbound row failed.
    struct PanickingProvider;
    #[async_trait]
    impl LLMProvider for PanickingProvider {
        async fn chat(
            &self,
            _m: &[Message],
            _t: &[ToolDefinition],
            _c: CancellationToken,
        ) -> Result<ChatResponse> {
            panic!("PanickingProvider: chat panic");
        }
        async fn chat_stream(
            &self,
            _m: &[Message],
            _t: &[ToolDefinition],
            _tx: tokio::sync::mpsc::Sender<StreamEvent>,
            _c: CancellationToken,
        ) -> Result<()> {
            panic!("PanickingProvider: chat_stream panic");
        }
        fn max_context(&self) -> usize {
            128_000
        }
    }

    fn test_config() -> Arc<Config> {
        Arc::new(Config::default())
    }

    /// Build an AgentMap whose builder produces fresh Agents backed by
    /// MockProviders that all return the same canned reply.
    fn agent_map_with_canned_reply(reply: &'static str) -> AgentMap {
        let builder: AgentBuilder = Arc::new(move |hooks: HookRegistry| {
            Box::pin(async move {
                let mock = Arc::new(MockProvider::new(128_000));
                mock.enqueue_text(reply);
                Ok(Agent::for_test(
                    Box::new(ProviderHandle(mock)),
                    ToolRegistry::new(),
                    hooks,
                    empty_skills(),
                ))
            })
        });
        AgentMap::with_builder(test_config(), Duration::from_secs(60), builder)
    }

    fn agent_map_with_panicking_provider() -> AgentMap {
        let builder: AgentBuilder = Arc::new(|hooks: HookRegistry| {
            Box::pin(async move {
                Ok(Agent::for_test(
                    Box::new(PanickingProvider),
                    ToolRegistry::new(),
                    hooks,
                    empty_skills(),
                ))
            })
        });
        AgentMap::with_builder(test_config(), Duration::from_secs(60), builder)
    }

    fn streaming_caps() -> PlatformCapabilities {
        PlatformCapabilities {
            supports_edit: true,
            edit_min_interval_ms: 0,
            ..PlatformCapabilities::default()
        }
    }

    #[tokio::test]
    async fn worker_runs_agent_and_enqueues_reply() {
        let db = fresh_db();
        let inbound = InboundQueue::new(db.clone());
        let outbound = Arc::new(OutboundQueue::new(db.clone(), 5));
        let render_registry = Arc::new(RenderRegistry::new());
        let agent_map = agent_map_with_canned_reply("hi back");

        let id = inbound
            .enqueue(InboundMessage {
                platform: "loopback".into(),
                chat_id: "c".into(),
                user_id: "u".into(),
                text: "hi".into(),
                message_id: None,
                reply_to: None,
                attachments: vec![],
            })
            .await
            .unwrap();
        let row = inbound.claim_next().await.unwrap().expect("row");
        assert_eq!(row.id, id);

        process_one(
            row,
            &agent_map,
            &inbound,
            &outbound,
            &render_registry,
            streaming_caps(),
        )
        .await
        .unwrap();

        // The streaming pipeline emits one (or more) outbound rows for
        // the canned "hi back" reply via the renderer. The first row
        // should carry the streamed text; pin that.
        let row = outbound
            .claim_due(chrono::Utc::now().timestamp())
            .await
            .unwrap()
            .expect("outbound row from renderer");
        assert_eq!(row.text, "hi back");
        assert_eq!(row.platform, "loopback");
        assert_eq!(row.chat_id, "c");
        assert!(
            row.turn_id.is_some(),
            "streaming row should carry a per-turn id"
        );
    }

    #[tokio::test]
    async fn worker_panic_marks_inbound_failed() {
        // YYC-137: with max_attempts=1, the first failure exhausts
        // the retry budget and the row lands in 'dead' rather than
        // looping back to 'pending'.
        let db = fresh_db();
        let inbound = crate::gateway::queue::InboundQueue::with_policy(db.clone(), 1, 60);
        let outbound = Arc::new(OutboundQueue::new(db.clone(), 5));
        let render_registry = Arc::new(RenderRegistry::new());
        let agent_map = agent_map_with_panicking_provider();

        inbound
            .enqueue(InboundMessage {
                platform: "loopback".into(),
                chat_id: "c".into(),
                user_id: "u".into(),
                text: "boom".into(),
                message_id: None,
                reply_to: None,
                attachments: vec![],
            })
            .await
            .unwrap();
        let row = inbound.claim_next().await.unwrap().expect("row");

        let res = process_one(
            row,
            &agent_map,
            &inbound,
            &outbound,
            &render_registry,
            streaming_caps(),
        )
        .await;
        assert!(
            res.is_err(),
            "process_one should propagate the panic as Err"
        );

        // Row exhausted retries → dead. Not claimable.
        assert!(inbound.claim_next().await.unwrap().is_none());
        assert_eq!(inbound.count_dead().await.unwrap(), 1);

        // Outbound should be empty (no reply enqueued — the panic fires
        // before any StreamEvent reaches the renderer).
        assert!(
            outbound
                .claim_due(chrono::Utc::now().timestamp())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn worker_panic_evicts_lane_so_next_request_builds_fresh_agent() {
        // YYC-133 acceptance pin: a panic during run_prompt_stream should
        // remove the lane's cached Agent. The next inbound row for the
        // same lane gets a freshly built Agent and runs cleanly.
        use std::sync::atomic::{AtomicUsize, Ordering};
        let db = fresh_db();
        let inbound = InboundQueue::new(db.clone());
        let outbound = Arc::new(OutboundQueue::new(db.clone(), 5));
        let render_registry = Arc::new(RenderRegistry::new());

        // Builder counts how many Agents it constructs. First call →
        // panicking provider; second call → canned-reply provider.
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_builder = Arc::clone(&calls);
        let builder: AgentBuilder = Arc::new(move |hooks: HookRegistry| {
            let n = calls_for_builder.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if n == 0 {
                    Ok(Agent::for_test(
                        Box::new(PanickingProvider),
                        ToolRegistry::new(),
                        hooks,
                        empty_skills(),
                    ))
                } else {
                    let mock = Arc::new(MockProvider::new(128_000));
                    mock.enqueue_text("recovered");
                    Ok(Agent::for_test(
                        Box::new(ProviderHandle(mock)),
                        ToolRegistry::new(),
                        hooks,
                        empty_skills(),
                    ))
                }
            })
        });
        let agent_map = AgentMap::with_builder(test_config(), Duration::from_secs(60), builder);

        // First row: panics. process_one should propagate the panic AND
        // evict the lane.
        inbound
            .enqueue(InboundMessage {
                platform: "loopback".into(),
                chat_id: "c".into(),
                user_id: "u".into(),
                text: "boom".into(),
                message_id: None,
                reply_to: None,
                attachments: vec![],
            })
            .await
            .unwrap();
        let row = inbound.claim_next().await.unwrap().expect("row");
        let res = process_one(
            row,
            &agent_map,
            &inbound,
            &outbound,
            &render_registry,
            streaming_caps(),
        )
        .await;
        assert!(res.is_err());

        // Lane should now be empty in the AgentMap (evicted).
        let lane = LaneKey {
            platform: "loopback".into(),
            chat_id: "c".into(),
        };
        assert!(
            !agent_map.inner().read().await.contains_key(&lane),
            "lane should have been evicted after panic"
        );

        // Second row: builder fires a second time, returns a clean
        // Agent, and the worker drives it to a streamed reply.
        inbound
            .enqueue(InboundMessage {
                platform: "loopback".into(),
                chat_id: "c".into(),
                user_id: "u".into(),
                text: "again".into(),
                message_id: None,
                reply_to: None,
                attachments: vec![],
            })
            .await
            .unwrap();
        let row = inbound.claim_next().await.unwrap().expect("row");
        process_one(
            row,
            &agent_map,
            &inbound,
            &outbound,
            &render_registry,
            streaming_caps(),
        )
        .await
        .unwrap();

        let reply = outbound
            .claim_due(chrono::Utc::now().timestamp())
            .await
            .unwrap()
            .expect("outbound row from recovered agent");
        assert_eq!(reply.text, "recovered");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "builder should have run twice — once for the panicking agent, once for the rebuild"
        );
    }
}
