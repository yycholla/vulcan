//! GET /v1/lanes — diagnostic snapshot of the gateway's lane → daemon-session
//! cache.
//!
//! Slice 3 Task 3.4 replaced the in-process per-lane Agent cache with
//! a [`DaemonLaneRouter`]: each chat lane maps to a daemon session id
//! and the daemon owns the Agent. This route surfaces only what the
//! router cache knows (lane → session_id triples). For richer
//! daemon-side state (idle-eviction timers, in-flight flags, agent
//! warm/cold) call the daemon's `session.list` directly.
use axum::Json;
use axum::extract::State;

use crate::gateway::server::AppState;

pub async fn handle(State(state): State<AppState>) -> Json<serde_json::Value> {
    let snapshot = state.lane_router.snapshot_cache();
    Json(serde_json::json!({ "lanes": snapshot }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::client::ClientError;
    use crate::gateway::lane_router::DaemonLaneRouter;
    use crate::gateway::registry::PlatformRegistry;
    use crate::gateway::server::{AppState, build_router};
    use crate::memory::DbPool;

    fn fresh_db() -> DbPool {
        crate::memory::in_memory_gateway_pool().expect("in-memory pool")
    }

    fn no_daemon_router() -> Arc<DaemonLaneRouter> {
        Arc::new(DaemonLaneRouter::with_client_factory(|| {
            Box::pin(async {
                Err(ClientError::Protocol(
                    "lanes test: client factory must not be invoked".into(),
                ))
            })
        }))
    }

    fn build_app_state(db: DbPool, lane_router: Arc<DaemonLaneRouter>) -> AppState {
        AppState {
            api_token: Arc::new("secret".into()),
            inbound: Arc::new(crate::gateway::queue::InboundQueue::new(db.clone())),
            outbound: Arc::new(crate::gateway::queue::OutboundQueue::new(db.clone(), 5)),
            registry: Arc::new(PlatformRegistry::new()),
            lane_router,
            scheduler_jobs: Arc::new(Vec::new()),
            scheduler_store: None,
        }
    }

    /// Empty router cache: `/v1/lanes` returns `{"lanes": []}`.
    #[tokio::test]
    async fn get_lanes_empty_cache_returns_empty_array() {
        let db = fresh_db();
        let state = build_app_state(db.clone(), no_daemon_router());

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
        assert!(lanes.is_empty(), "fresh router has no cached lanes");
    }
}
