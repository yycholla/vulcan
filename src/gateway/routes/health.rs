use axum::Json;

pub async fn handle() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}
