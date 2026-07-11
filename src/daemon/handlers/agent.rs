//! Handlers for the `agent.*` method namespace.
//!
//! Slice 3: agent operations (`status`, `switch_model`, `list_models`)
//! target the per-session warm Agent installed on the request's
//! [`SessionState`]. Returns `SESSION_NOT_FOUND` if the envelope's
//! `session` field doesn't match any live session, and
//! `AGENT_BUILD_FAILED` if lazy-building the session Agent fails.

use serde_json::json;

use crate::agent::ModelSelection;
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

async fn resolve_for_provider_switch(
    state: &DaemonState,
    session_id: &str,
    profile: Option<&str>,
    model_override: Option<&str>,
) -> Result<AgentHandle, ProtocolError> {
    let Some(sess) = state.sessions().get(session_id) else {
        return Err(ProtocolError {
            code: "SESSION_NOT_FOUND".into(),
            message: format!("session '{session_id}' not found"),
            retryable: false,
        });
    };
    sess.touch();
    sess.ensure_agent_for_provider_switch(&state.session_agent_assembler(), profile, model_override)
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

fn selection_payload(sel: ModelSelection) -> serde_json::Value {
    json!({
        "id": sel.model.id,
        "display_name": sel.model.display_name,
        "context_length": sel.model.context_length,
        "pricing": sel.pricing,
        "features": sel.model.features,
        "top_provider": sel.model.top_provider,
        "max_context": sel.max_context,
    })
}

fn models_payload(models: Vec<crate::provider::catalog::ModelInfo>) -> serde_json::Value {
    json!({
        "models": models.into_iter().map(|m| json!({
            "id": m.id,
            "display_name": m.display_name,
            "context_length": m.context_length,
        })).collect::<Vec<_>>(),
    })
}

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
        Ok(sel) => Response::ok(id, selection_payload(sel)),
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

pub async fn switch_provider(
    state: &DaemonState,
    id: String,
    session_id: String,
    profile: Option<&str>,
) -> Response {
    let config = state.config();
    if let Some(name) = profile
        && !config.providers.contains_key(name)
    {
        return Response::error(
            id,
            ProtocolError {
                code: "SWITCH_PROVIDER_FAILED".into(),
                message: format!("Provider profile '{name}' not found in config"),
                retryable: false,
            },
        );
    }
    let agent_arc = match resolve_for_provider_switch(state, &session_id, profile, None).await {
        Ok(a) => a,
        Err(e) if e.code == "AGENT_BUILD_FAILED" => {
            return Response::error(
                id,
                ProtocolError {
                    code: "SWITCH_PROVIDER_FAILED".into(),
                    message: e.message,
                    retryable: false,
                },
            );
        }
        Err(e) => return Response::error(id, e),
    };
    let mut agent = agent_arc.lock().await;
    match agent.switch_provider(profile, &config).await {
        Ok(sel) => Response::ok(id, selection_payload(sel)),
        Err(e) => Response::error(
            id,
            ProtocolError {
                code: "SWITCH_PROVIDER_FAILED".into(),
                message: format!("{e}"),
                retryable: false,
            },
        ),
    }
}

pub async fn switch_provider_model(
    state: &DaemonState,
    id: String,
    session_id: String,
    profile: Option<&str>,
    model: &str,
) -> Response {
    let config = state.config();
    if let Some(name) = profile
        && !config.providers.contains_key(name)
    {
        return Response::error(
            id,
            ProtocolError {
                code: "SWITCH_PROVIDER_MODEL_FAILED".into(),
                message: format!("Provider profile '{name}' not found in config"),
                retryable: false,
            },
        );
    }
    let agent_arc =
        match resolve_for_provider_switch(state, &session_id, profile, Some(model)).await {
            Ok(a) => a,
            Err(e) if e.code == "AGENT_BUILD_FAILED" => {
                return Response::error(
                    id,
                    ProtocolError {
                        code: "SWITCH_PROVIDER_MODEL_FAILED".into(),
                        message: e.message,
                        retryable: false,
                    },
                );
            }
            Err(e) => return Response::error(id, e),
        };
    let mut agent = agent_arc.lock().await;
    match agent.switch_provider_model(profile, &config, model).await {
        Ok(sel) => Response::ok(id, selection_payload(sel)),
        Err(e) => Response::error(
            id,
            ProtocolError {
                code: "SWITCH_PROVIDER_MODEL_FAILED".into(),
                message: format!("{e}"),
                retryable: false,
            },
        ),
    }
}

// -- agent.list_models --

pub async fn list_models(state: &DaemonState, id: String, session_id: String) -> Response {
    let Some(sess) = state.sessions().get(&session_id) else {
        return Response::error(
            id,
            ProtocolError {
                code: "SESSION_NOT_FOUND".into(),
                message: format!("session '{session_id}' not found"),
                retryable: false,
            },
        );
    };
    sess.touch();

    let models = if let Some(agent_arc) = sess.agent_arc() {
        let agent = agent_arc.lock().await;
        agent.available_models().await
    } else {
        catalog_models_for_config(&state.config()).await
    };

    match models {
        Ok(models) => Response::ok(id, models_payload(models)),
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

async fn catalog_models_for_config(
    config: &crate::config::Config,
) -> anyhow::Result<Vec<crate::provider::catalog::ModelInfo>> {
    use std::time::Duration;

    let provider = config.active_provider_config();
    if provider.disable_catalog {
        return Ok(Vec::new());
    }

    let api_key = match config.api_key_for(provider) {
        Some(k) => k,
        None if crate::agent::is_local_base_url(&provider.base_url) => String::new(),
        None => {
            anyhow::bail!(
                "No API key configured. Set VULCAN_API_KEY or add api_key to ~/.vulcan/config.toml"
            );
        }
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let ttl = Duration::from_secs(provider.catalog_cache_ttl_hours * 3600);
    let catalog = crate::provider::catalog::for_base_url(client, &provider.base_url, &api_key, ttl);
    catalog.list_models().await.map_err(Into::into)
}
