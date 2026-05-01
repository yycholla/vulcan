use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;

use crate::gateway::server::AppState;
use crate::platform::InboundMessage;

#[derive(Deserialize)]
pub struct InboundRequest {
    pub platform: String,
    pub chat_id: String,
    pub user_id: String,
    pub text: String,
}

pub async fn handle(
    State(state): State<AppState>,
    Json(body): Json<InboundRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    // Reject unknown platforms early so unsuited messages don't pollute the queue.
    if state.registry.get(&body.platform).is_none() {
        tracing::warn!(target: "gateway::inbound",
            platform = %body.platform, "unknown platform rejected");
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("unknown platform: {}", body.platform)})),
        ));
    }

    let msg = InboundMessage {
        platform: body.platform,
        chat_id: body.chat_id,
        user_id: body.user_id,
        text: body.text,
        scheduler_job_id: None,
        message_id: None,
        reply_to: None,
        attachments: vec![],
    };
    match state.inbound.enqueue(msg).await {
        Ok(id) => Ok((StatusCode::ACCEPTED, Json(serde_json::json!({"id": id})))),
        Err(e) => {
            tracing::error!(target: "gateway::inbound", error = %e, "inbound enqueue failed");
            Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "inbound enqueue failed"})),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::gateway::loopback::LoopbackPlatform;
    use crate::gateway::registry::PlatformRegistry;
    use crate::gateway::server::{AppState, build_router};
    use crate::memory::DbPool;

    fn fresh_db() -> DbPool {
        crate::memory::in_memory_gateway_pool().expect("in-memory pool")
    }

    fn no_daemon_client() -> Arc<crate::gateway::daemon_client::GatewayDaemonClient> {
        Arc::new(
            crate::gateway::daemon_client::GatewayDaemonClient::with_client_factory(|| {
                Box::pin(async {
                    Err(crate::client::ClientError::Protocol(
                        "inbound test: client factory must not be invoked".into(),
                    ))
                })
            }),
        )
    }

    fn app_state_with(registry: PlatformRegistry, db: DbPool) -> AppState {
        // Build an AppState pointing at the given db + registry. The
        // /v1/inbound route only enqueues; it never opens a daemon
        // client, so a no-daemon router is fine here.
        AppState {
            api_token: Arc::new("secret".into()),
            inbound: Arc::new(crate::gateway::queue::InboundQueue::new(db.clone())),
            outbound: Arc::new(crate::gateway::queue::OutboundQueue::new(db.clone(), 5)),
            registry: Arc::new(registry),
            lane_router: Arc::new(crate::gateway::lane_router::DaemonLaneRouter::new()),
            daemon_client: no_daemon_client(),
            scheduler_jobs: Arc::new(Vec::new()),
            scheduler_store: None,
        }
    }

    fn registry_with_loopback() -> PlatformRegistry {
        let mut reg = PlatformRegistry::new();
        reg.register("loopback", Arc::new(LoopbackPlatform::default()));
        reg
    }

    fn auth_request(uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("authorization", "Bearer secret")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn post_inbound_enqueues_and_returns_id() {
        let db = fresh_db();
        let state = app_state_with(registry_with_loopback(), db.clone());
        let inbound_q = Arc::clone(&state.inbound);
        let app = build_router(state);

        let resp = app
            .oneshot(auth_request(
                "/v1/inbound",
                serde_json::json!({
                    "platform": "loopback",
                    "chat_id": "c1",
                    "user_id": "u1",
                    "text": "hi"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        // Confirm the row landed and can be claimed.
        let row = inbound_q.claim_next().await.unwrap().expect("row");
        assert_eq!(row.text, "hi");
        assert_eq!(row.platform, "loopback");
    }

    #[tokio::test]
    async fn post_inbound_unknown_platform_returns_400() {
        let db = fresh_db();
        let state = app_state_with(PlatformRegistry::new(), db); // empty registry
        let app = build_router(state);
        let resp = app
            .oneshot(auth_request(
                "/v1/inbound",
                serde_json::json!({
                    "platform": "nope",
                    "chat_id": "c",
                    "user_id": "u",
                    "text": "x"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn post_inbound_missing_bearer_returns_401() {
        let db = fresh_db();
        let state = app_state_with(registry_with_loopback(), db);
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/inbound")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "platform": "loopback",
                            "chat_id": "c",
                            "user_id": "u",
                            "text": "x"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn post_inbound_db_error_returns_503() {
        // Force the inbound enqueue to fail. Approach: build a pool against a
        // FRESH in-memory connection that has NOT had the schema applied, so
        // the INSERT into `inbound_queue` errors with "no such table".
        let unschemed: DbPool = r2d2::Pool::builder()
            .max_size(1)
            .build(r2d2_sqlite::SqliteConnectionManager::memory())
            .expect("unschemed in-memory pool");
        // Build the state pointing at the unschemed db.
        let state = AppState {
            api_token: Arc::new("secret".into()),
            inbound: Arc::new(crate::gateway::queue::InboundQueue::new(unschemed.clone())),
            outbound: Arc::new(crate::gateway::queue::OutboundQueue::new(
                unschemed.clone(),
                5,
            )),
            registry: Arc::new(registry_with_loopback()),
            lane_router: Arc::new(crate::gateway::lane_router::DaemonLaneRouter::new()),
            daemon_client: no_daemon_client(),
            scheduler_jobs: Arc::new(Vec::new()),
            scheduler_store: None,
        };
        let app = build_router(state);
        let resp = app
            .oneshot(auth_request(
                "/v1/inbound",
                serde_json::json!({
                    "platform": "loopback",
                    "chat_id": "c",
                    "user_id": "u",
                    "text": "x"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
