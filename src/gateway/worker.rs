//! Lane worker — pulls a claimed inbound row, drives the per-lane Agent,
//! and enqueues the reply for the outbound delivery loop.
//!
//! The lane router (`lane.rs`) hands one inbound row at a time to
//! `process_one`. We deliberately catch panics from `Agent::run_prompt` so a
//! single wedged Agent doesn't kill the whole lane worker task — the row gets
//! marked `failed` and the worker loop survives to claim the next row.

use crate::gateway::agent_map::AgentMap;
use crate::gateway::lane::LaneKey;
use crate::gateway::queue::{InboundQueue, InboundRow, OutboundQueue};
use crate::platform::OutboundMessage;

use futures_util::FutureExt;
use std::panic::AssertUnwindSafe;

/// Drive one inbound row through its Agent and enqueue the reply.
///
/// Steps:
/// 1. Look up or spawn the Agent for the row's lane.
/// 2. Run the prompt; catch panics so the lane worker survives.
/// 3. On success: enqueue the reply on the outbound queue and mark the
///    inbound row `done`.
/// 4. On failure (Err or panic): mark the inbound row `failed` and bubble
///    the error up to the lane worker for logging.
pub async fn process_one(
    row: InboundRow,
    agent_map: &AgentMap,
    inbound_queue: &InboundQueue,
    _outbound_queue: &OutboundQueue,
) -> anyhow::Result<()> {
    let lane = LaneKey {
        platform: row.platform.clone(),
        chat_id: row.chat_id.clone(),
    };

    // AssertUnwindSafe: we don't read Agent state after a panic — we just
    // mark the inbound row failed and drop our handle. The Agent's own state
    // may be inconsistent, but the next get_or_spawn after eviction will
    // build a fresh one. This is the same trade-off catch_unwind always
    // makes; the alternative (poisoning the lane forever) is worse.
    let result: anyhow::Result<String> = AssertUnwindSafe(async {
        let agent = agent_map.get_or_spawn(&lane).await?;
        let mut agent = agent.lock().await;
        agent.run_prompt(&row.text).await
    })
    .catch_unwind()
    .await
    .unwrap_or_else(|payload| {
        let msg = payload
            .downcast_ref::<&'static str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        Err(anyhow::anyhow!(
            "agent panicked while running prompt: {msg}"
        ))
    });

    match result {
        Ok(reply) => {
            inbound_queue
                .complete_with_outbound(
                    row.id,
                    OutboundMessage {
                        platform: row.platform,
                        chat_id: row.chat_id,
                        text: reply,
                        attachments: vec![],
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use anyhow::Result;
    use async_trait::async_trait;
    use rusqlite::Connection;
    use tokio_util::sync::CancellationToken;

    use crate::agent::Agent;
    use crate::config::Config;
    use crate::gateway::agent_map::{AgentBuilder, AgentMap};
    use crate::gateway::queue::{InboundQueue, OutboundQueue};
    use crate::hooks::HookRegistry;
    use crate::platform::InboundMessage;
    use crate::provider::mock::MockProvider;
    use crate::provider::{ChatResponse, LLMProvider, Message, StreamEvent, ToolDefinition};
    use crate::skills::SkillRegistry;
    use crate::tools::ToolRegistry;

    fn fresh_db() -> Arc<StdMutex<Connection>> {
        let c = Connection::open_in_memory().expect("open mem db");
        crate::memory::initialize_test_conn(&c).expect("schema");
        Arc::new(StdMutex::new(c))
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
            tx: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
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
            _tx: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
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

    #[tokio::test]
    async fn worker_runs_agent_and_enqueues_reply() {
        let db = fresh_db();
        let inbound = InboundQueue::new(Arc::clone(&db));
        let outbound = OutboundQueue::new(Arc::clone(&db), 5);
        let agent_map = agent_map_with_canned_reply("hi back");

        let id = inbound
            .enqueue(InboundMessage {
                platform: "loopback".into(),
                chat_id: "c".into(),
                user_id: "u".into(),
                text: "hi".into(),
            })
            .await
            .unwrap();
        let row = inbound.claim_next().await.unwrap().expect("row");
        assert_eq!(row.id, id);

        process_one(row, &agent_map, &inbound, &outbound)
            .await
            .unwrap();

        let row = outbound
            .claim_due(chrono::Utc::now().timestamp())
            .await
            .unwrap()
            .expect("outbound row");
        assert_eq!(row.text, "hi back");
        assert_eq!(row.platform, "loopback");
        assert_eq!(row.chat_id, "c");
    }

    #[tokio::test]
    async fn worker_panic_marks_inbound_failed() {
        let db = fresh_db();
        let inbound = InboundQueue::new(Arc::clone(&db));
        let outbound = OutboundQueue::new(Arc::clone(&db), 5);
        let agent_map = agent_map_with_panicking_provider();

        inbound
            .enqueue(InboundMessage {
                platform: "loopback".into(),
                chat_id: "c".into(),
                user_id: "u".into(),
                text: "boom".into(),
            })
            .await
            .unwrap();
        let row = inbound.claim_next().await.unwrap().expect("row");

        let res = process_one(row, &agent_map, &inbound, &outbound).await;
        assert!(
            res.is_err(),
            "process_one should propagate the panic as Err"
        );

        // Inbound row should be in 'failed' state — claim_next returns None.
        assert!(inbound.claim_next().await.unwrap().is_none());

        // Outbound should be empty (no reply enqueued).
        assert!(
            outbound
                .claim_due(chrono::Utc::now().timestamp())
                .await
                .unwrap()
                .is_none()
        );
    }
}
