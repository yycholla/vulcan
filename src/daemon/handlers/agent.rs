//! Handlers for the `agent.*` method namespace.
//!
//! Slice 3: agent operations (`status`, `switch_model`, `list_models`)
//! target the per-session warm Agent installed on the request's
//! [`SessionState`]. Returns `SESSION_NOT_FOUND` if the envelope's
//! `session` field doesn't match any live session, and
//! `AGENT_NOT_AVAILABLE` if the session exists but its Agent hasn't
//! been built yet.

use serde_json::json;

use crate::daemon::protocol::{ProtocolError, Response};
use crate::daemon::session::AgentHandle;
use crate::daemon::state::DaemonState;

/// Resolve the per-session AgentHandle, lazy-building if absent.
/// Returns `SESSION_NOT_FOUND` when the session doesn't exist and
/// `AGENT_BUILD_FAILED` when the inline build errors. Pre-Task-3.3
/// this returned `AGENT_NOT_AVAILABLE` for the no-agent case.
async fn resolve(state: &DaemonState, session_id: &str) -> Result<AgentHandle, ProtocolError> {
    let Some(sess) = state.sessions().get(session_id) else {
        return Err(ProtocolError {
            code: "SESSION_NOT_FOUND".into(),
            message: format!("session '{session_id}' not found"),
            retryable: false,
        });
    };
    sess.touch();
    sess.ensure_agent(&state.session_agent_assembler())
        .await
        .map_err(|e| ProtocolError {
            code: "AGENT_BUILD_FAILED".into(),
            message: format!("agent build for session '{session_id}' failed: {e}"),
            retryable: true,
        })
}

// -- agent.status --

pub async fn status(state: &DaemonState, id: String, session_id: String) -> Response {
    let agent_arc = match resolve(state, &session_id).await {
        Ok(a) => a,
        Err(e) => return Response::error(id, e),
    };
    let agent = agent_arc.lock().await;
    Response::ok(
        id,
        json!({
            "model": agent.active_model(),
            "session_id": agent.session_id(),
            "turns": agent.iterations(),
            "provider": agent.active_profile(),
            "max_context": agent.max_context(),
        }),
    )
}

// -- agent.switch_model --

pub async fn switch_model(
    state: &DaemonState,
    id: String,
    session_id: String,
    model: &str,
) -> Response {
    let agent_arc = match resolve(state, &session_id).await {
        Ok(a) => a,
        Err(e) => return Response::error(id, e),
    };
    let mut agent = agent_arc.lock().await;
    match agent.switch_model(model).await {
        Ok(sel) => Response::ok(
            id,
            json!({
                "id": sel.model.id,
                "display_name": sel.model.display_name,
                "context_length": sel.model.context_length,
                "max_context": sel.max_context,
            }),
        ),
        Err(e) => Response::error(
            id,
            ProtocolError {
                code: "SWITCH_MODEL_FAILED".into(),
                message: format!("{e}"),
                retryable: false,
            },
        ),
    }
}

// -- agent.list_models --

pub async fn list_models(state: &DaemonState, id: String, session_id: String) -> Response {
    let agent_arc = match resolve(state, &session_id).await {
        Ok(a) => a,
        Err(e) => return Response::error(id, e),
    };
    let agent = agent_arc.lock().await;
    match agent.available_models().await {
        Ok(models) => Response::ok(
            id,
            json!({
                "models": models.into_iter().map(|m| json!({
                    "id": m.id,
                    "display_name": m.display_name,
                    "context_length": m.context_length,
                })).collect::<Vec<_>>(),
            }),
        ),
        Err(e) => Response::error(
            id,
            ProtocolError {
                code: "CATALOG_FETCH_FAILED".into(),
                message: format!("{e}"),
                retryable: false,
            },
        ),
    }
}
