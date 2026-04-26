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
}

/// Build the axum router. Public so tests can drive it via `tower::ServiceExt::oneshot`.
///
/// Topology: `/health` is unauthenticated; everything under `/v1/*` is wrapped
/// in a bearer-auth middleware. The middleware lives on the nested `/v1`
/// router (not the outer one) so the public `/health` endpoint isn't affected.
pub fn build_router(state: AppState) -> Router {
    let v1 = Router::new()
        .route(
            "/lanes",
            axum::routing::get(crate::gateway::routes::lanes::handle),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            bearer_auth,
        ));

    Router::new()
        .route(
            "/health",
            axum::routing::get(crate::gateway::routes::health::handle),
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
    if provided != state.api_token.as_str() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use rusqlite::Connection;
    use tower::ServiceExt;

    use crate::config::Config;
    use crate::gateway::agent_map::AgentMap;

    fn fresh_db() -> Arc<StdMutex<Connection>> {
        let c = Connection::open_in_memory().expect("open mem db");
        crate::memory::initialize_test_conn(&c).expect("schema");
        Arc::new(StdMutex::new(c))
    }

    fn test_app_state(token: &str) -> AppState {
        let config = Arc::new(Config::default());
        let agent_map = AgentMap::new(config, Duration::from_secs(60));
        let db = fresh_db();
        AppState {
            api_token: Arc::new(token.into()),
            inbound: Arc::new(crate::gateway::queue::InboundQueue::new(Arc::clone(&db))),
            outbound: Arc::new(crate::gateway::queue::OutboundQueue::new(Arc::clone(&db), 5)),
            registry: Arc::new(crate::gateway::registry::PlatformRegistry::new()),
            agent_map: Arc::new(agent_map),
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
