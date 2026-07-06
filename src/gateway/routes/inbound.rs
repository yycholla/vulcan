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
