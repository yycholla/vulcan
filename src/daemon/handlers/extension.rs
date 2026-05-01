//! Handlers for live daemon extension lifecycle operations.

use serde_json::json;

use crate::daemon::protocol::{ProtocolError, Response};
use crate::daemon::state::DaemonState;
use crate::extensions::ExtensionStatus;

pub async fn enable(state: &DaemonState, req_id: String, extension_id: &str) -> Response {
    let Some(pool) = state.pool() else {
        return Response::error(req_id, no_pool_error());
    };
    let registry = pool.extension_registry();
    if registry.get(extension_id).is_none() {
        return Response::error(req_id, not_found(extension_id));
    }
    registry.set_status(extension_id, ExtensionStatus::Active);

    let mut attached_sessions = Vec::new();
    for session_id in state.sessions().ids() {
        let Some(session) = state.sessions().get(&session_id) else {
            continue;
        };
        let Some(agent_handle) = session.agent_arc() else {
            continue;
        };
        let mut agent = agent_handle.lock().await;
        match agent
            .attach_session_extension(extension_id, &registry)
            .await
        {
            Ok(true) => attached_sessions.push(session_id),
            Ok(false) => {}
            Err(err) => {
                return Response::error(
                    req_id,
                    ProtocolError {
                        code: "EXTENSION_ENABLE_FAILED".into(),
                        message: format!("enable {extension_id} failed: {err}"),
                        retryable: true,
                    },
                );
            }
        }
    }

    Response::ok(
        req_id,
        json!({
            "ok": true,
            "extension_id": extension_id,
            "attached_sessions": attached_sessions,
        }),
    )
}

pub async fn disable(state: &DaemonState, req_id: String, extension_id: &str) -> Response {
    let Some(pool) = state.pool() else {
        return Response::error(req_id, no_pool_error());
    };
    let registry = pool.extension_registry();
    if registry.get(extension_id).is_none() {
        return Response::error(req_id, not_found(extension_id));
    }
    registry.set_status(extension_id, ExtensionStatus::Inactive);

    let mut drained_sessions = Vec::new();
    for session_id in state.sessions().ids() {
        let Some(session) = state.sessions().get(&session_id) else {
            continue;
        };
        let Some(agent_handle) = session.agent_arc() else {
            continue;
        };
        let agent = agent_handle.lock().await;
        if agent.drain_session_extension(extension_id) {
            drained_sessions.push(session_id);
        }
    }

    Response::ok(
        req_id,
        json!({
            "ok": true,
            "extension_id": extension_id,
            "drained_sessions": drained_sessions,
        }),
    )
}

pub async fn kill(state: &DaemonState, req_id: String, extension_id: &str) -> Response {
    let Some(pool) = state.pool() else {
        return Response::error(req_id, no_pool_error());
    };
    let registry = pool.extension_registry();
    if registry.get(extension_id).is_none() {
        return Response::error(req_id, not_found(extension_id));
    }
    registry.set_status(extension_id, ExtensionStatus::Inactive);

    let mut killed_sessions = Vec::new();
    for session_id in state.sessions().ids() {
        let Some(session) = state.sessions().get(&session_id) else {
            continue;
        };
        let Some(agent_handle) = session.agent_arc() else {
            continue;
        };
        let mut agent = agent_handle.lock().await;
        if agent.kill_session_extension(extension_id).await {
            killed_sessions.push(session_id);
        }
    }

    Response::ok(
        req_id,
        json!({
            "ok": true,
            "extension_id": extension_id,
            "killed_sessions": killed_sessions,
            "warning": "may break in-flight tool calls",
        }),
    )
}

fn no_pool_error() -> ProtocolError {
    ProtocolError {
        code: "EXTENSION_POOL_UNAVAILABLE".into(),
        message: "daemon runtime pool is not installed".into(),
        retryable: true,
    }
}

fn not_found(extension_id: &str) -> ProtocolError {
    ProtocolError {
        code: "EXTENSION_NOT_FOUND".into(),
        message: format!("extension `{extension_id}` not installed"),
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::extensions::api::{DaemonCodeExtension, SessionExtension, SessionExtensionCtx};
    use crate::extensions::{ExtensionMetadata, ExtensionSource};

    struct LifecycleExt;

    impl DaemonCodeExtension for LifecycleExt {
        fn metadata(&self) -> ExtensionMetadata {
            let mut meta = ExtensionMetadata::new(
                "lifecycle-ext",
                "Lifecycle Ext",
                "0.1.0",
                ExtensionSource::Builtin,
            );
            meta.status = ExtensionStatus::Active;
            meta
        }

        fn instantiate(&self, _ctx: SessionExtensionCtx) -> Arc<dyn SessionExtension> {
            struct Session;
            impl SessionExtension for Session {}
            Arc::new(Session)
        }
    }

    fn test_agent() -> crate::agent::Agent {
        crate::agent::Agent::for_test(
            Box::new(crate::provider::mock::MockProvider::new(4096)),
            crate::tools::ToolRegistry::new(),
            crate::hooks::HookRegistry::new(),
            Arc::new(crate::skills::SkillRegistry::empty()),
        )
    }

    fn state_with_extension() -> DaemonState {
        let pool = Arc::new(crate::runtime_pool::RuntimeResourcePool::for_tests());
        pool.extension_registry()
            .register_daemon_extension(Arc::new(LifecycleExt));
        let state = DaemonState::for_tests_minimal().with_pool(pool);
        let main = state.sessions().get("main").unwrap();
        main.set_agent(test_agent());
        state
    }

    #[tokio::test]
    async fn enable_attaches_extension_to_live_agent() {
        let state = state_with_extension();
        let resp = enable(&state, "req-1".into(), "lifecycle-ext").await;
        assert!(resp.error.is_none(), "{:?}", resp.error);
        let main = state.sessions().get("main").unwrap();
        let agent = main.agent_arc().unwrap();
        let agent = agent.lock().await;
        assert_eq!(agent.session_extension_ids(), vec!["lifecycle-ext"]);
    }

    #[tokio::test]
    async fn kill_removes_extension_from_live_agent() {
        let state = state_with_extension();
        let _ = enable(&state, "req-1".into(), "lifecycle-ext").await;
        let resp = kill(&state, "req-2".into(), "lifecycle-ext").await;
        assert!(resp.error.is_none(), "{:?}", resp.error);
        let main = state.sessions().get("main").unwrap();
        let agent = main.agent_arc().unwrap();
        let agent = agent.lock().await;
        assert!(agent.session_extension_ids().is_empty());
    }
}
