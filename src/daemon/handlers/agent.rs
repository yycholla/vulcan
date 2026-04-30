//! Handlers for the `agent.*` method namespace.
//!
//! Slice 2: inspect and mutate the shared Agent held in [`DaemonState`].

use serde_json::json;

use crate::daemon::protocol::{ProtocolError, Response};
use crate::daemon::state::DaemonState;

fn no_agent(id: String) -> Response {
    Response::error(
        id,
        ProtocolError {
            code: "AGENT_NOT_AVAILABLE".into(),
            message: "agent not initialized in daemon".into(),
            retryable: false,
        },
    )
}

// -- agent.status --

pub async fn status(state: &DaemonState, id: String) -> Response {
    let Some(agent_arc) = state.agent() else {
        return no_agent(id);
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

pub async fn switch_model(state: &DaemonState, id: String, model: &str) -> Response {
    let Some(agent_arc) = state.agent() else {
        return no_agent(id);
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

pub async fn list_models(state: &DaemonState, id: String) -> Response {
    let Some(agent_arc) = state.agent() else {
        return no_agent(id);
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
