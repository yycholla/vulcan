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
