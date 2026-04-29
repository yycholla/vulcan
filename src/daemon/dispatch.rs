//! Method router. Translates a [`Request`] into a [`Response`] by
//! delegating to the appropriate handler module under
//! [`crate::daemon::handlers`].

use std::sync::Arc;

use crate::daemon::handlers::{cortex, daemon_ops};
use crate::daemon::protocol::{ProtocolError, Request, Response};
use crate::daemon::state::DaemonState;

pub struct Dispatcher {
    state: Arc<DaemonState>,
}

impl Dispatcher {
    pub fn new(state: Arc<DaemonState>) -> Self {
        Self { state }
    }

    pub async fn dispatch(&self, req: Request) -> Response {
        match req.method.as_str() {
            "daemon.ping" => daemon_ops::ping(&self.state, req.id).await,
            "daemon.shutdown" => {
                let force = req
                    .params
                    .get("force")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                daemon_ops::shutdown(&self.state, req.id, force).await
            }
            "daemon.reload" => daemon_ops::reload(&self.state, req.id).await,
            "daemon.status" => daemon_ops::status(&self.state, req.id).await,
            // ── Cortex ──
            "cortex.store" => {
                let text = req
                    .params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let importance = req
                    .params
                    .get("importance")
                    .and_then(|v| v.as_f64())
                    .map(|f| f as f32);
                cortex::store(&self.state, req.id, text, importance).await
            }
            "cortex.search" => {
                let query = req
                    .params
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let limit = req
                    .params
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10) as usize;
                cortex::search(&self.state, req.id, query, limit).await
            }
            "cortex.stats" => cortex::stats(&self.state, req.id).await,
            "cortex.recall" => {
                let limit = req
                    .params
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20) as usize;
                cortex::recall(&self.state, req.id, limit).await
            }
            "cortex.seed" => {
                let sessions = req
                    .params
                    .get("sessions")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(3) as usize;
                cortex::seed(&self.state, req.id, sessions).await
            }
            "cortex.edges_from" => {
                let node_id = req
                    .params
                    .get("node_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                cortex::edges_from(&self.state, req.id, node_id).await
            }
            "cortex.edges_to" => {
                let node_id = req
                    .params
                    .get("node_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                cortex::edges_to(&self.state, req.id, node_id).await
            }
            "cortex.delete_edge" => {
                let edge_id = req
                    .params
                    .get("edge_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                cortex::delete_edge(&self.state, req.id, edge_id).await
            }
            "cortex.run_decay" => cortex::run_decay(&self.state, req.id).await,
            other => Response::error(
                req.id,
                ProtocolError {
                    code: "UNKNOWN_METHOD".into(),
                    message: format!("unknown method: {other}"),
                    retryable: false,
                },
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::protocol::*;
    use std::sync::Arc;

    fn req(method: &str) -> Request {
        Request {
            version: 1,
            id: format!("test-{method}"),
            session: "main".into(),
            method: method.into(),
            params: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn ping_dispatches_to_daemon_ops() {
        let dispatcher = Dispatcher::new(Arc::new(crate::daemon::state::DaemonState::new()));
        let resp = dispatcher.dispatch(req("daemon.ping")).await;
        let result = resp.result.expect("ping returns ok");
        assert_eq!(result["pong"], true);
        assert!(result["pid"].is_number());
        assert!(result["uptime_secs"].is_number());
        assert!(resp.error.is_none());
    }

    #[tokio::test]
    async fn unknown_method_returns_unknown_method_error() {
        let dispatcher = Dispatcher::new(Arc::new(crate::daemon::state::DaemonState::new()));
        let resp = dispatcher.dispatch(req("does.not.exist")).await;
        assert!(resp.result.is_none());
        let err = resp.error.expect("error returned");
        assert_eq!(err.code, "UNKNOWN_METHOD");
        assert!(err.message.contains("does.not.exist"));
    }

    #[tokio::test]
    async fn shutdown_signals_state() {
        let state = Arc::new(crate::daemon::state::DaemonState::new());
        let mut signal = state.shutdown_signal();
        let dispatcher = Dispatcher::new(state);

        // Note: with watch, no yield_now needed — the channel is
        // latching, so even if `changed()` is awaited AFTER the
        // dispatch fires, it resolves immediately for receivers that
        // were acquired BEFORE the send.
        let resp = dispatcher.dispatch(req("daemon.shutdown")).await;
        assert_eq!(resp.result.unwrap()["ok"], true);

        tokio::time::timeout(std::time::Duration::from_millis(500), signal.changed())
            .await
            .expect("shutdown must propagate")
            .expect("watch sender alive");
        assert!(*signal.borrow(), "watch latched true");
    }

    #[tokio::test]
    async fn reload_queues_reload_signal() {
        let state = Arc::new(crate::daemon::state::DaemonState::new());
        let signal = state.reload_signal();
        let dispatcher = Dispatcher::new(state);

        let waiter = tokio::spawn(async move {
            signal.notified().await;
        });
        // Shutdown moved to watch (latching); reload still uses Notify
        // because it's multi-fire and Task 0.10 (config_watch) will
        // replace the whole reload pipeline anyway. Notify only wakes
        // already-parked waiters, so yield to let the waiter task
        // register before `notify_waiters` fires.
        tokio::task::yield_now().await;

        let resp = dispatcher.dispatch(req("daemon.reload")).await;
        assert_eq!(resp.result.unwrap()["ok"], true);

        tokio::time::timeout(std::time::Duration::from_millis(500), waiter)
            .await
            .expect("reload signal must fire")
            .unwrap();
    }

    #[tokio::test]
    async fn status_includes_pid_uptime_sessions() {
        let dispatcher = Dispatcher::new(Arc::new(crate::daemon::state::DaemonState::new()));
        let resp = dispatcher.dispatch(req("daemon.status")).await;
        let result = resp.result.expect("status ok");
        assert!(result["pid"].is_number());
        assert!(result["uptime_secs"].is_number());
        assert!(
            result["sessions"].is_array(),
            "sessions should be array (Slice 0: empty)"
        );
    }

    #[tokio::test]
    async fn cortex_store_without_cortex_returns_disabled_error() {
        let dispatcher = Dispatcher::new(Arc::new(crate::daemon::state::DaemonState::new()));
        let mut r = req("cortex.store");
        r.params = serde_json::json!({ "text": "hello", "importance": 0.5 });
        let resp = dispatcher.dispatch(r).await;
        assert!(resp.result.is_none());
        let err = resp.error.expect("error returned");
        assert_eq!(err.code, "CORTEX_DISABLED");
    }

    #[tokio::test]
    async fn cortex_stats_without_cortex_returns_disabled_error() {
        let dispatcher = Dispatcher::new(Arc::new(crate::daemon::state::DaemonState::new()));
        let resp = dispatcher.dispatch(req("cortex.stats")).await;
        let err = resp.error.expect("error returned");
        assert_eq!(err.code, "CORTEX_DISABLED");
    }
}
