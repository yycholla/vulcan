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
            #[cfg(test)]
            "test.slow_stream" => {
                let (frame_tx, frame_rx) = mpsc::channel(4);
                let (done_tx, done_rx) = oneshot::channel();
                let req_id = req.id;
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    let _ = frame_tx
                        .send(StreamFrame {
                            version: 1,
                            id: Some(req_id.clone()),
                            stream: "text".into(),
                            data: serde_json::json!({ "text": "slow" }),
                        })
                        .await;
                    let _ = done_tx.send(Response::ok(req_id, serde_json::json!({ "done": true })));
                });
                DispatchResult::Stream {
                    frames: frame_rx,
                    done: done_rx,
                }
            }

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
            "agent.status" => {
                let session = req.session.clone();
                DispatchResult::Response(agent::status(&self.state, req.id, session).await)
            }
            "agent.switch_model" => {
                let session = req.session.clone();
                let model = req
                    .params
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                DispatchResult::Response(
                    agent::switch_model(&self.state, req.id, session, model).await,
                )
            }
            "agent.list_models" => {
                let session = req.session.clone();
                DispatchResult::Response(agent::list_models(&self.state, req.id, session).await)
            }

            // -- Prompt --
            "prompt.run" => {
                let session = req.session.clone();
                let input = req
                    .params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                DispatchResult::Response(prompt::run(&self.state, req.id, session, input).await)
            }
            "prompt.stream" => {
                let session = req.session.clone();
                let input = req
                    .params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let (frames, done) = prompt::stream(&self.state, req.id, session, input);
                DispatchResult::Stream { frames, done }
            }
            "prompt.cancel" => {
                let session = req.session.clone();
                DispatchResult::Response(prompt::cancel(&self.state, req.id, session).await)
            }

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
            "cortex.prompt.create" => {
                let name = req
                    .params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let body = req
                    .params
                    .get("body")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                DispatchResult::Response(
                    cortex::prompt_create(&self.state, req.id, name, body).await,
                )
            }
            "cortex.prompt.get" => {
                let name = req
                    .params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                DispatchResult::Response(cortex::prompt_get(&self.state, req.id, name).await)
            }
            "cortex.prompt.list" => {
                DispatchResult::Response(cortex::prompt_list(&self.state, req.id).await)
            }
            "cortex.prompt.set" => {
                let name = req
                    .params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let body = req
                    .params
                    .get("body")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                DispatchResult::Response(cortex::prompt_set(&self.state, req.id, name, body).await)
            }
            "cortex.prompt.remove" => {
                let name = req
                    .params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                DispatchResult::Response(cortex::prompt_remove(&self.state, req.id, name).await)
            }
            "cortex.prompt.migrate" => {
                let entries = req
                    .params
                    .get("entries")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                DispatchResult::Response(cortex::prompt_migrate(&self.state, req.id, entries).await)
            }
            "cortex.prompt.performance" => {
                let name = req
                    .params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                DispatchResult::Response(
                    cortex::prompt_performance(&self.state, req.id, name).await,
                )
            }

            // -- Session --
            "session.list" => DispatchResult::Response(session::list(&self.state, req.id).await),
            "session.create" => {
                let id = req
                    .params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let resume_from = req
                    .params
                    .get("resume_from")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                DispatchResult::Response(
                    session::create(&self.state, req.id, id, resume_from).await,
                )
            }
            "session.destroy" => {
                let sid = req
                    .params
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                DispatchResult::Response(session::destroy(&self.state, req.id, sid).await)
            }
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
    use crate::config::{Config, CortexConfig};
    use crate::daemon::protocol::*;
    use crate::memory::cortex::CortexStore;
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

    fn req_with_params(method: &str, params: serde_json::Value) -> Request {
        Request {
            version: 1,
            id: format!("test-{method}"),
            session: "main".into(),
            method: method.into(),
            params,
        }
    }

    fn cortex_state() -> (tempfile::TempDir, Arc<crate::daemon::state::DaemonState>) {
        let dir = tempfile::tempdir().unwrap();
        let config = CortexConfig {
            enabled: true,
            db_path: Some(dir.path().join("cortex.redb")),
            ..CortexConfig::default()
        };
        let store = CortexStore::try_open(&config).expect("open cortex");
        let mut daemon_config = Config::default();
        daemon_config.cortex = config;
        let state =
            crate::daemon::state::DaemonState::new(Arc::new(daemon_config)).with_cortex(store);
        (dir, Arc::new(state))
    }

    #[tokio::test]
    async fn ping_dispatches_to_daemon_ops() {
        let dispatcher = Dispatcher::new(Arc::new(
            crate::daemon::state::DaemonState::for_tests_minimal(),
        ));
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
        let dispatcher = Dispatcher::new(Arc::new(
            crate::daemon::state::DaemonState::for_tests_minimal(),
        ));
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
        let state = Arc::new(crate::daemon::state::DaemonState::for_tests_minimal());
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
        let state = Arc::new(crate::daemon::state::DaemonState::for_tests_minimal());
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
        let dispatcher = Dispatcher::new(Arc::new(
            crate::daemon::state::DaemonState::for_tests_minimal(),
        ));
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
        let dispatcher = Dispatcher::new(Arc::new(
            crate::daemon::state::DaemonState::for_tests_minimal(),
        ));
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
        let dispatcher = Dispatcher::new(Arc::new(
            crate::daemon::state::DaemonState::for_tests_minimal(),
        ));
        match dispatcher.dispatch(req("cortex.stats")).await {
            DispatchResult::Response(resp) => {
                let err = resp.error.expect("error returned");
                assert_eq!(err.code, "CORTEX_DISABLED");
            }
            DispatchResult::Stream { .. } => panic!("cortex.stats should not stream"),
        }
    }

    #[tokio::test]
    async fn cortex_prompt_create_and_list_use_daemon_store() {
        let (_dir, state) = cortex_state();
        let dispatcher = Dispatcher::new(state);

        let create = req_with_params(
            "cortex.prompt.create",
            serde_json::json!({
                "name": "daily",
                "body": "summarize the day",
            }),
        );
        match dispatcher.dispatch(create).await {
            DispatchResult::Response(resp) => {
                assert!(resp.error.is_none(), "got {:?}", resp.error);
                let result = resp.result.expect("prompt create result");
                assert_eq!(result["name"], "daily");
                assert!(result["node_id"].as_str().is_some());
            }
            DispatchResult::Stream { .. } => panic!("cortex.prompt.create should not stream"),
        }

        match dispatcher.dispatch(req("cortex.prompt.list")).await {
            DispatchResult::Response(resp) => {
                assert!(resp.error.is_none(), "got {:?}", resp.error);
                let prompts = resp.result.unwrap()["prompts"].as_array().unwrap().clone();
                assert_eq!(prompts.len(), 1);
                assert_eq!(prompts[0]["title"], "daily");
                assert_eq!(prompts[0]["body"], "summarize the day");
            }
            DispatchResult::Stream { .. } => panic!("cortex.prompt.list should not stream"),
        }
    }

    #[tokio::test]
    async fn agent_status_without_agent_attempts_lazy_build() {
        // Task 3.3: agent.* now lazy-builds via ensure_agent. With
        // for_tests_minimal()'s default Config (no real provider),
        // build fails → AGENT_BUILD_FAILED instead of the pre-Task-3.3
        // AGENT_NOT_AVAILABLE.
        let dispatcher = Dispatcher::new(Arc::new(
            crate::daemon::state::DaemonState::for_tests_minimal(),
        ));
        match dispatcher.dispatch(req("agent.status")).await {
            DispatchResult::Response(resp) => {
                assert!(resp.result.is_none());
                let err = resp.error.expect("error returned");
                assert_eq!(err.code, "AGENT_BUILD_FAILED");
            }
            DispatchResult::Stream { .. } => panic!("agent.status should not stream"),
        }
    }

    #[tokio::test]
    async fn prompt_run_without_agent_attempts_lazy_build() {
        // Task 3.3: prompt.run lazy-builds; build fails on the
        // for_tests_minimal default config → AGENT_BUILD_FAILED.
        let dispatcher = Dispatcher::new(Arc::new(
            crate::daemon::state::DaemonState::for_tests_minimal(),
        ));
        let mut r = req("prompt.run");
        r.params = serde_json::json!({ "text": "hello" });
        match dispatcher.dispatch(r).await {
            DispatchResult::Response(resp) => {
                assert!(resp.result.is_none());
                let err = resp.error.expect("error returned");
                assert_eq!(err.code, "AGENT_BUILD_FAILED");
            }
            DispatchResult::Stream { .. } => panic!("prompt.run should not stream"),
        }
    }
}
