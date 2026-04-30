//! Method router. Translates a [`Request`] into a [`Response`] by
//! delegating to the appropriate handler module under
//! [`crate::daemon::handlers`].

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use crate::daemon::handlers::{agent, approval, cortex, daemon_ops, prompt, session};
use crate::daemon::protocol::{ProtocolError, Request, Response, StreamFrame};
use crate::daemon::state::DaemonState;

/// Result of dispatching a request -- either a single response or a
/// streaming response with incremental frames and a final result.
pub enum DispatchResult {
    Response(Response),
    Stream {
        frames: mpsc::Receiver<StreamFrame>,
        done: oneshot::Receiver<Response>,
    },
}

pub struct Dispatcher {
    state: Arc<DaemonState>,
}

impl Dispatcher {
    pub fn new(state: Arc<DaemonState>) -> Self {
        Self { state }
    }

    pub async fn dispatch(&self, req: Request) -> DispatchResult {
        match req.method.as_str() {
            // -- Daemon --
            "daemon.ping" => DispatchResult::Response(daemon_ops::ping(&self.state, req.id).await),
            "daemon.shutdown" => {
                let force = req
                    .params
                    .get("force")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                DispatchResult::Response(daemon_ops::shutdown(&self.state, req.id, force).await)
            }
            "daemon.reload" => {
                DispatchResult::Response(daemon_ops::reload(&self.state, req.id).await)
            }
            "daemon.status" => {
                DispatchResult::Response(daemon_ops::status(&self.state, req.id).await)
            }

            // -- Agent --
            "agent.status" => DispatchResult::Response(agent::status(&self.state, req.id).await),
            "agent.switch_model" => {
                let model = req
                    .params
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                DispatchResult::Response(agent::switch_model(&self.state, req.id, model).await)
            }
            "agent.list_models" => {
                DispatchResult::Response(agent::list_models(&self.state, req.id).await)
            }

            // -- Prompt --
            "prompt.run" => {
                let input = req
                    .params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                DispatchResult::Response(prompt::run(&self.state, req.id, input).await)
            }
            "prompt.stream" => {
                let input = req
                    .params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let (frames, done) = prompt::stream(&self.state, req.id, input);
                DispatchResult::Stream { frames, done }
            }
            "prompt.cancel" => DispatchResult::Response(prompt::cancel(&self.state, req.id).await),

            // -- Cortex --
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
                DispatchResult::Response(cortex::store(&self.state, req.id, text, importance).await)
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
                DispatchResult::Response(cortex::search(&self.state, req.id, query, limit).await)
            }
            "cortex.stats" => DispatchResult::Response(cortex::stats(&self.state, req.id).await),
            "cortex.recall" => {
                let limit = req
                    .params
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20) as usize;
                DispatchResult::Response(cortex::recall(&self.state, req.id, limit).await)
            }
            "cortex.seed" => {
                let sessions = req
                    .params
                    .get("sessions")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(3) as usize;
                DispatchResult::Response(cortex::seed(&self.state, req.id, sessions).await)
            }
            "cortex.edges_from" => {
                let node_id = req
                    .params
                    .get("node_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                DispatchResult::Response(cortex::edges_from(&self.state, req.id, node_id).await)
            }
            "cortex.edges_to" => {
                let node_id = req
                    .params
                    .get("node_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                DispatchResult::Response(cortex::edges_to(&self.state, req.id, node_id).await)
            }
            "cortex.delete_edge" => {
                let edge_id = req
                    .params
                    .get("edge_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                DispatchResult::Response(cortex::delete_edge(&self.state, req.id, edge_id).await)
            }
            "cortex.run_decay" => {
                DispatchResult::Response(cortex::run_decay(&self.state, req.id).await)
            }

            // -- Session --
            "session.list" => DispatchResult::Response(session::list(&self.state, req.id).await),
            "session.search" => {
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
                DispatchResult::Response(session::search(&self.state, req.id, query, limit).await)
            }
            "session.resume" => {
                let sid = req
                    .params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                DispatchResult::Response(session::resume(&self.state, req.id, sid).await)
            }
            "session.history" => {
                let sid = req
                    .params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                DispatchResult::Response(session::history(&self.state, req.id, sid).await)
            }

            // -- Approval --
            "approval.pending" => {
                DispatchResult::Response(approval::pending(&self.state, req.id).await)
            }
            "approval.respond" => {
                let decision = req
                    .params
                    .get("decision")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                DispatchResult::Response(approval::respond(&self.state, req.id, decision).await)
            }

            other => DispatchResult::Response(Response::error(
                req.id,
                ProtocolError {
                    code: "UNKNOWN_METHOD".into(),
                    message: format!("unknown method: {other}"),
                    retryable: false,
                },
            )),
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
        match dispatcher.dispatch(req("daemon.ping")).await {
            DispatchResult::Response(resp) => {
                let result = resp.result.expect("ping returns ok");
                assert_eq!(result["pong"], true);
                assert!(result["pid"].is_number());
                assert!(result["uptime_secs"].is_number());
                assert!(resp.error.is_none());
            }
            DispatchResult::Stream { .. } => panic!("ping should not stream"),
        }
    }

    #[tokio::test]
    async fn unknown_method_returns_unknown_method_error() {
        let dispatcher = Dispatcher::new(Arc::new(crate::daemon::state::DaemonState::new()));
        match dispatcher.dispatch(req("does.not.exist")).await {
            DispatchResult::Response(resp) => {
                assert!(resp.result.is_none());
                let err = resp.error.expect("error returned");
                assert_eq!(err.code, "UNKNOWN_METHOD");
                assert!(err.message.contains("does.not.exist"));
            }
            DispatchResult::Stream { .. } => panic!("unknown method should not stream"),
        }
    }

    #[tokio::test]
    async fn shutdown_signals_state() {
        let state = Arc::new(crate::daemon::state::DaemonState::new());
        let mut signal = state.shutdown_signal();
        let dispatcher = Dispatcher::new(state);

        let resp = match dispatcher.dispatch(req("daemon.shutdown")).await {
            DispatchResult::Response(r) => r,
            DispatchResult::Stream { .. } => panic!("shutdown should not stream"),
        };
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
        tokio::task::yield_now().await;

        let resp = match dispatcher.dispatch(req("daemon.reload")).await {
            DispatchResult::Response(r) => r,
            DispatchResult::Stream { .. } => panic!("reload should not stream"),
        };
        assert_eq!(resp.result.unwrap()["ok"], true);

        tokio::time::timeout(std::time::Duration::from_millis(500), waiter)
            .await
            .expect("reload signal must fire")
            .unwrap();
    }

    #[tokio::test]
    async fn status_includes_pid_uptime_sessions() {
        let dispatcher = Dispatcher::new(Arc::new(crate::daemon::state::DaemonState::new()));
        let resp = match dispatcher.dispatch(req("daemon.status")).await {
            DispatchResult::Response(r) => r,
            DispatchResult::Stream { .. } => panic!("status should not stream"),
        };
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
        match dispatcher.dispatch(r).await {
            DispatchResult::Response(resp) => {
                assert!(resp.result.is_none());
                let err = resp.error.expect("error returned");
                assert_eq!(err.code, "CORTEX_DISABLED");
            }
            DispatchResult::Stream { .. } => panic!("cortex.store should not stream"),
        }
    }

    #[tokio::test]
    async fn cortex_stats_without_cortex_returns_disabled_error() {
        let dispatcher = Dispatcher::new(Arc::new(crate::daemon::state::DaemonState::new()));
        match dispatcher.dispatch(req("cortex.stats")).await {
            DispatchResult::Response(resp) => {
                let err = resp.error.expect("error returned");
                assert_eq!(err.code, "CORTEX_DISABLED");
            }
            DispatchResult::Stream { .. } => panic!("cortex.stats should not stream"),
        }
    }

    #[tokio::test]
    async fn agent_status_without_agent_returns_noagent_error() {
        let dispatcher = Dispatcher::new(Arc::new(crate::daemon::state::DaemonState::new()));
        match dispatcher.dispatch(req("agent.status")).await {
            DispatchResult::Response(resp) => {
                assert!(resp.result.is_none());
                let err = resp.error.expect("error returned");
                assert_eq!(err.code, "AGENT_NOT_AVAILABLE");
            }
            DispatchResult::Stream { .. } => panic!("agent.status should not stream"),
        }
    }

    #[tokio::test]
    async fn prompt_run_without_agent_returns_noagent_error() {
        let dispatcher = Dispatcher::new(Arc::new(crate::daemon::state::DaemonState::new()));
        let mut r = req("prompt.run");
        r.params = serde_json::json!({ "text": "hello" });
        match dispatcher.dispatch(r).await {
            DispatchResult::Response(resp) => {
                assert!(resp.result.is_none());
                let err = resp.error.expect("error returned");
                assert_eq!(err.code, "AGENT_NOT_AVAILABLE");
            }
            DispatchResult::Stream { .. } => panic!("prompt.run should not stream"),
        }
    }
}
