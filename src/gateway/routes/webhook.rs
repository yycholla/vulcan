use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};

use crate::gateway::server::AppState;

pub async fn handle(
    State(state): State<AppState>,
    Path(platform): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    let plat = state.registry.get(&platform).ok_or_else(|| {
        tracing::warn!(target: "gateway::webhook",
            platform = %platform,
            "webhook for unknown platform");
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": format!("unknown platform: {platform}")})),
        )
    })?;

    let inbound = plat.verify_webhook(&headers, &body).await.map_err(|e| {
        tracing::warn!(target: "gateway::webhook",
            platform = %platform,
            error = %e,
            "webhook verification failed");
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "invalid webhook signature"})),
        )
    })?;

    state.inbound.enqueue(inbound).await.map_err(|e| {
        tracing::error!(target: "gateway::webhook", error = %e, "inbound enqueue failed");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "inbound enqueue failed"})),
        )
    })?;

    Ok((StatusCode::OK, Json(serde_json::json!({"status": "ok"}))))
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

    fn sign_loopback(secret: &str, body: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = <Hmac<Sha256>>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let bytes = mac.finalize().into_bytes();
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn app_state_with(registry: PlatformRegistry, db: DbPool) -> AppState {
        let config = Arc::new(crate::config::Config::default());
        let agent_map =
            crate::gateway::agent_map::AgentMap::new(config, std::time::Duration::from_secs(60));
        AppState {
            api_token: Arc::new("secret".into()),
            inbound: Arc::new(crate::gateway::queue::InboundQueue::new(db.clone())),
            outbound: Arc::new(crate::gateway::queue::OutboundQueue::new(db.clone(), 5)),
            registry: Arc::new(registry),
            agent_map: Arc::new(agent_map),
        }
    }

    #[tokio::test]
    async fn webhook_loopback_accepts_signed_request() {
        let db = fresh_db();
        let mut reg = PlatformRegistry::new();
        let lp = Arc::new(LoopbackPlatform::with_webhook_secret("hush"));
        reg.register("loopback", lp.clone());
        let state = app_state_with(reg, db.clone());
        let inbound_q = Arc::clone(&state.inbound);
        let app = build_router(state);

        let body = serde_json::json!({"chat_id": "c", "user_id": "u", "text": "hi"}).to_string();
        let signature = sign_loopback("hush", body.as_bytes());

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhook/loopback")
                    .header("content-type", "application/json")
                    .header("x-loopback-signature", signature)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let row = inbound_q.claim_next().await.unwrap().expect("row");
        assert_eq!(row.platform, "loopback");
        assert_eq!(row.text, "hi");
    }

    #[tokio::test]
    async fn webhook_loopback_rejects_invalid_signature() {
        let db = fresh_db();
        let mut reg = PlatformRegistry::new();
        reg.register(
            "loopback",
            Arc::new(LoopbackPlatform::with_webhook_secret("hush")),
        );
        let state = app_state_with(reg, db);
        let app = build_router(state);

        let body = serde_json::json!({"chat_id": "c", "user_id": "u", "text": "hi"}).to_string();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhook/loopback")
                    .header("content-type", "application/json")
                    .header("x-loopback-signature", "deadbeef")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn webhook_unknown_platform_404() {
        let db = fresh_db();
        let state = app_state_with(PlatformRegistry::new(), db);
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhook/nope")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn webhook_rejects_oversized_body_before_verification() {
        let db = fresh_db();
        let mut reg = PlatformRegistry::new();
        reg.register(
            "loopback",
            Arc::new(LoopbackPlatform::with_webhook_secret("hush")),
        );
        let state = app_state_with(reg, db);
        let app = build_router(state);

        let oversized = "x".repeat(70 * 1024);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhook/loopback")
                    .header("content-type", "application/json")
                    .header("x-loopback-signature", "irrelevant")
                    .body(Body::from(oversized))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }
}
