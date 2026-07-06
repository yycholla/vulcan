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
