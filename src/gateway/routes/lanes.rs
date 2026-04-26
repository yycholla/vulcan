// Placeholder for Task 15. Real implementation will surface active lanes from
// `AppState::agent_map`; today this returns an empty array so Task 13 can test
// bearer auth against a `/v1/*` route.
use axum::Json;
use axum::extract::State;

use crate::gateway::server::AppState;

pub async fn handle(State(_state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!([]))
}
