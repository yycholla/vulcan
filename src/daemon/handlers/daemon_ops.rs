//! Handlers for the `daemon.*` method namespace.
//!
//! These are pure-state operations that don't touch the agent, cortex,
//! or session machinery. They land in Slice 0 so the daemon process
//! has health/lifecycle endpoints from day 1.

use serde_json::json;

use crate::daemon::protocol::Response;
use crate::daemon::state::DaemonState;

pub async fn ping(state: &DaemonState, id: String) -> Response {
    Response::ok(
        id,
        json!({
            "pong": true,
            "pid": std::process::id(),
            "uptime_secs": state.uptime_secs(),
        }),
    )
}

pub async fn shutdown(state: &DaemonState, id: String, _force: bool) -> Response {
    state.signal_shutdown();
    Response::ok(id, json!({ "ok": true }))
}

pub async fn reload(state: &DaemonState, id: String) -> Response {
    let report = state.reload_from_disk().await;
    Response::ok(
        id,
        serde_json::to_value(report).unwrap_or_else(|_| json!({ "ok": false })),
    )
}

pub async fn status(state: &DaemonState, id: String) -> Response {
    Response::ok(
        id,
        json!({
            "pid": std::process::id(),
            "uptime_secs": state.uptime_secs(),
            "reloads_applied": state.reloads_applied(),
            "last_reload": state.last_reload_report(),
            "runtime_resources": runtime_resources_status(state),
            "sessions": state.session_descriptors(),
        }),
    )
}

fn runtime_resources_status(state: &DaemonState) -> serde_json::Value {
    match state.pool() {
        Some(pool) if pool.is_degraded() => json!({
            "status": "degraded",
            "degraded": pool.degraded_resources(),
        }),
        Some(_) => json!({
            "status": "ok",
            "degraded": [],
        }),
        None => json!({
            "status": "not_initialized",
            "degraded": [],
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::DaemonState;
    use std::sync::Arc;

    #[tokio::test]
    async fn ping_returns_response_with_pong() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        let resp = ping(&state, "id-1".into()).await;
        assert_eq!(resp.id, "id-1");
        let r = resp.result.unwrap();
        assert_eq!(r["pong"], true);
    }

    #[tokio::test]
    async fn status_surfaces_degraded_runtime_resources() {
        let pool = Arc::new(
            crate::runtime_pool::RuntimeResourcePool::for_tests_degraded(
                "session_store",
                "sqlite unavailable; using in-memory session history",
            ),
        );
        let state = Arc::new(DaemonState::for_tests_minimal().with_pool(pool));

        let resp = status(&state, "status-1".into()).await;
        let r = resp.result.unwrap();
        assert_eq!(r["runtime_resources"]["status"], "degraded");
        assert_eq!(
            r["runtime_resources"]["degraded"][0]["component"],
            "session_store"
        );
    }

    #[tokio::test]
    async fn shutdown_signal_latches() {
        // Verify the watch-based shutdown is both idempotent (multiple
        // calls don't panic) and latching (a receiver acquired AFTER
        // the signal still observes the true value via borrow()).
        let s = DaemonState::for_tests_minimal();
        s.signal_shutdown();
        s.signal_shutdown();
        let rx = s.shutdown_signal();
        assert!(*rx.borrow(), "late receiver sees latched shutdown");
    }
}
