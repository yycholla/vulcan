//! GET /v1/lanes — observability snapshot of currently-active per-chat lanes.
//!
//! Returns `{"lanes": [LaneSnapshot, ...]}` sorted most-recent first by
//! `last_activity`. Reads only what `AgentMap` already tracks; the per-lane
//! audit ring is intentionally not exposed here (different shape — see
//! `agent_map.rs::LaneEntry.audit_buf`).
use axum::Json;
use axum::extract::State;

use crate::gateway::server::AppState;

pub async fn handle(State(state): State<AppState>) -> Json<serde_json::Value> {
    let snapshot = state.agent_map.snapshot().await;
    Json(serde_json::json!({ "lanes": snapshot }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;
    use std::time::{Duration, Instant};

    use anyhow::Result;
    use async_trait::async_trait;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use rusqlite::Connection;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use crate::agent::Agent;
    use crate::config::Config;
    use crate::gateway::agent_map::AgentMap;
    use crate::gateway::lane::LaneKey;
    use crate::gateway::registry::PlatformRegistry;
    use crate::gateway::server::{AppState, build_router};
    use crate::hooks::HookRegistry;
    use crate::hooks::audit::AuditHook;
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

    /// Wraps an Arc<MockProvider> so multiple Agents can share a mock
    /// instance. Mirrors the same shim in `worker.rs::tests` and
    /// `agent_map.rs::tests` — three uses justifies a future helper, but
    /// keeping local for Task 15.
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

    /// Build an Agent backed by `MockProvider`. The route under test never
    /// invokes the agent — it only reads `last_activity` and `session_id`
    /// from the map — so no canned reply is needed.
    fn build_test_agent() -> Arc<tokio::sync::Mutex<Agent>> {
        let mock = Arc::new(MockProvider::new(128_000));
        Arc::new(tokio::sync::Mutex::new(Agent::for_test(
            Box::new(ProviderHandle(mock)),
            ToolRegistry::new(),
            HookRegistry::new(),
            empty_skills(),
        )))
    }

    fn build_app_state(db: Arc<StdMutex<Connection>>) -> AppState {
        let config = Arc::new(Config::default());
        let agent_map = AgentMap::new(config, Duration::from_secs(60));
        AppState {
            api_token: Arc::new("secret".into()),
            inbound: Arc::new(crate::gateway::queue::InboundQueue::new(Arc::clone(&db))),
            outbound: Arc::new(crate::gateway::queue::OutboundQueue::new(
                Arc::clone(&db),
                5,
            )),
            registry: Arc::new(PlatformRegistry::new()),
            agent_map: Arc::new(agent_map),
        }
    }

    #[tokio::test]
    async fn get_lanes_lists_active_lanes() {
        let db = fresh_db();
        let state = build_app_state(Arc::clone(&db));

        let now = Instant::now();
        let lane_a = LaneKey {
            platform: "loopback".into(),
            chat_id: "a".into(),
        };
        let lane_b = LaneKey {
            platform: "loopback".into(),
            chat_id: "b".into(),
        };
        let agent_a = build_test_agent();
        let agent_b = build_test_agent();
        let (_h1, buf_a) = AuditHook::new(8);
        let (_h2, buf_b) = AuditHook::new(8);
        state
            .agent_map
            .insert_for_test(lane_a.clone(), agent_a, buf_a, now)
            .await;
        state
            .agent_map
            .insert_for_test(
                lane_b.clone(),
                agent_b,
                buf_b,
                now - Duration::from_secs(10),
            )
            .await;

        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/lanes")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let lanes = json
            .get("lanes")
            .and_then(|v| v.as_array())
            .expect("lanes array");
        assert_eq!(lanes.len(), 2);

        // Lane A is most-recent → should be first.
        assert_eq!(lanes[0]["chat_id"], "a");
        assert_eq!(lanes[1]["chat_id"], "b");
        assert!(
            lanes[0]["session_id"]
                .as_str()
                .unwrap()
                .starts_with("gateway:loopback:")
        );
        assert!(lanes[1]["last_activity_secs_ago"].as_u64().unwrap() >= 10);
    }
}
