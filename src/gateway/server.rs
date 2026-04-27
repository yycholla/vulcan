use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use crate::gateway::agent_map::AgentMap;
use crate::gateway::queue::{InboundQueue, OutboundQueue};
use crate::gateway::registry::PlatformRegistry;

/// Shared state passed to every route via `axum::extract::State`.
#[derive(Clone)]
pub struct AppState {
    pub api_token: Arc<String>,
    pub inbound: Arc<InboundQueue>,
    pub outbound: Arc<OutboundQueue>,
    pub registry: Arc<PlatformRegistry>,
    pub agent_map: Arc<AgentMap>,
    /// YYC-17 PR-4: declared scheduler jobs (from `Config.scheduler`).
    /// Empty when the gateway runs without the scheduler enabled.
    /// Owned via `Arc` so the route handler can clone the slice
    /// without copying job bodies.
    pub scheduler_jobs: Arc<Vec<crate::config::SchedulerJobConfig>>,
    /// YYC-17 PR-4: persistent run history. `None` when the
    /// scheduler isn't enabled, so the `/v1/scheduler` endpoint
    /// can answer with just the declared jobs and no run history.
    pub scheduler_store: Option<crate::gateway::scheduler_store::SchedulerStore>,
}

/// Build the axum router. Public so tests can drive it via `tower::ServiceExt::oneshot`.
///
/// Topology: `/health` is unauthenticated; everything under `/v1/*` is wrapped
/// in a bearer-auth middleware. The middleware lives on the nested `/v1`
/// router (not the outer one) so the public `/health` endpoint isn't affected.
/// Maximum body size for `/v1/*` JSON requests. Keeps a single oversized
/// payload from filling the SQLite queue. Webhook routes (Task 16) may
/// override this per-platform if a connector publishes large payloads.
const V1_BODY_LIMIT_BYTES: usize = 64 * 1024;
const WEBHOOK_BODY_LIMIT_BYTES: usize = 64 * 1024;

pub fn build_router(state: AppState) -> Router {
    let v1 = Router::new()
        .route(
            "/lanes",
            axum::routing::get(crate::gateway::routes::lanes::handle),
        )
        .route(
            "/inbound",
            axum::routing::post(crate::gateway::routes::inbound::handle),
        )
        .route(
            "/scheduler",
            axum::routing::get(crate::gateway::routes::scheduler::handle),
        )
        .layer(axum::extract::DefaultBodyLimit::max(V1_BODY_LIMIT_BYTES))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            bearer_auth,
        ));

    Router::new()
        .route(
            "/health",
            axum::routing::get(crate::gateway::routes::health::handle),
        )
        // Webhook routes live OUTSIDE the `/v1` bearer-auth nest — webhook auth
        // is per-platform HMAC, not the daemon's API token.
        .route(
            "/webhook/{platform}",
            axum::routing::post(crate::gateway::routes::webhook::handle).layer(
                axum::extract::DefaultBodyLimit::max(WEBHOOK_BODY_LIMIT_BYTES),
            ),
        )
        .nest("/v1", v1)
        .with_state(state)
}

/// Verify the `Authorization: Bearer <token>` header matches `state.api_token`.
/// Missing header, wrong scheme, or wrong token all return 401.
async fn bearer_auth(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let Some(provided) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    // Constant-time compare: avoids leaking the prefix length of the
    // configured token via early-out on byte mismatch.
    use subtle::ConstantTimeEq;
    if provided
        .as_bytes()
        .ct_eq(state.api_token.as_bytes())
        .unwrap_u8()
        == 0
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::config::Config;
    use crate::gateway::agent_map::AgentMap;
    use crate::memory::DbPool;

    fn fresh_db() -> DbPool {
        crate::memory::in_memory_gateway_pool().expect("in-memory pool")
    }

    fn test_app_state(token: &str) -> AppState {
        let config = Arc::new(Config::default());
        let agent_map = AgentMap::new(config, Duration::from_secs(60));
        let db = fresh_db();
        AppState {
            api_token: Arc::new(token.into()),
            inbound: Arc::new(crate::gateway::queue::InboundQueue::new(db.clone())),
            outbound: Arc::new(crate::gateway::queue::OutboundQueue::new(db.clone(), 5)),
            registry: Arc::new(crate::gateway::registry::PlatformRegistry::new()),
            agent_map: Arc::new(agent_map),
            scheduler_jobs: Arc::new(Vec::new()),
            scheduler_store: None,
        }
    }

    #[tokio::test]
    async fn health_endpoint_no_auth() {
        let app = build_router(test_app_state("secret"));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn bearer_required_returns_401_when_missing() {
        let app = build_router(test_app_state("secret"));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/lanes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn bearer_wrong_token_returns_401() {
        let app = build_router(test_app_state("secret"));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/lanes")
                    .header("authorization", "Bearer wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn bearer_correct_token_passes() {
        let app = build_router(test_app_state("secret"));
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
    }
}
