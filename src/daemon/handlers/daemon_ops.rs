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
    state.queue_reload();
    Response::ok(id, json!({ "ok": true }))
}

pub async fn status(state: &DaemonState, id: String) -> Response {
    Response::ok(
        id,
        json!({
            "pid": std::process::id(),
            "uptime_secs": state.uptime_secs(),
            "sessions": state.session_descriptors(),
        }),
    )
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
