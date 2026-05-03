//! Daemon listener and per-connection handling.
//!
//! Owns the [`SocketBinder`] and [`Dispatcher`]; spawns one task per
//! accepted connection that reads request frames, parses them with
//! [`parse_request_strict`], dispatches, and writes responses. Shutdown
//! is observed via the latching watch receiver in [`DaemonState`].

use std::path::Path;
use std::sync::Arc;

use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tracing::Instrument;

use crate::daemon::dispatch::{DispatchResult, Dispatcher};
use crate::daemon::lifecycle::SocketBinder;
use crate::daemon::protocol::{
    Response, parse_request_strict, read_frame_bytes, write_frame_bytes,
};
use crate::daemon::state::DaemonState;
use crate::observability;

/// Long-lived daemon server. Holds the bound socket and per-process
/// state for the lifetime of a single `vulcan daemon` process.
pub struct Server {
    binder: SocketBinder,
    dispatcher: Arc<Dispatcher>,
    state: Arc<DaemonState>,
}

impl Server {
    /// Bind to `path` (0600 perms; rejects live sockets, replaces stale
    /// leftovers) and prepare to serve.
    pub async fn bind(path: &Path, state: Arc<DaemonState>) -> std::io::Result<Self> {
        let binder = SocketBinder::bind(path).await?;
        let dispatcher = Arc::new(Dispatcher::new(state.clone()));
        Ok(Self {
            binder,
            dispatcher,
            state,
        })
    }

    /// Run the accept loop until shutdown is observed.
    ///
    /// On shutdown, this function returns. Per-connection tasks were
    /// spawned detached; whether they get to finish in-flight work depends
    /// on whether the surrounding tokio runtime keeps spinning after
    /// `run()` returns. On a runtime drop they are cancelled at any
    /// await point — graceful drain is a Slice 1+ concern (see YYC-266
    /// followup ticket on `JoinSet`-based shutdown).
    pub async fn run(self) {
        let mut shutdown = self.state.shutdown_signal();

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::info!("daemon: shutdown observed, stopping accept loop");
                        return;
                    }
                }
                accept = self.binder.listener().accept() => {
                    match accept {
                        Ok((stream, _addr)) => {
                            let dispatcher = self.dispatcher.clone();
                            tokio::spawn(handle_connection(stream, dispatcher));
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "daemon: accept failed");
                        }
                    }
                }
            }
        }
    }
}

async fn handle_connection(stream: UnixStream, dispatcher: Arc<Dispatcher>) {
    let (mut read_half, mut write_half) = stream.into_split();
    let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(32);
    let writer = tokio::spawn(async move {
        while let Some(body) = write_rx.recv().await {
            if let Err(e) = write_frame_bytes(&mut write_half, &body).await {
                tracing::warn!(error = %e, "daemon: write failed; dropping connection");
                break;
            }
        }
    });

    loop {
        let body = match read_frame_bytes(&mut read_half).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => {
                tracing::warn!(error = %e, "daemon: read failed; dropping connection");
                break;
            }
        };

        match parse_request_strict(&body) {
            Ok(req) => {
                let dispatcher = Arc::clone(&dispatcher);
                let write_tx = write_tx.clone();
                let span = observability::daemon_request_span(&req.method, &req.session, &req.id);
                let record_span = span.clone();
                tokio::spawn(
                    async move {
                        let response = dispatcher.dispatch(req).await;
                        write_dispatch_result(write_tx, response, &record_span).await;
                    }
                    .instrument(span),
                );
            }
            Err(proto_err) => {
                let span =
                    observability::daemon_request_span("parse_request", "unknown", "unknown");
                let response =
                    DispatchResult::Response(Response::error("unknown".into(), proto_err));
                write_dispatch_result(write_tx.clone(), response, &span).await;
            }
        }
    }

    drop(write_tx);
    writer.abort();
}

async fn write_dispatch_result(
    write_tx: mpsc::Sender<Vec<u8>>,
    response: DispatchResult,
    span: &tracing::Span,
) {
    match response {
        DispatchResult::Response(resp) => {
            record_response_outcome(span, &resp);
            send_body(&write_tx, &resp, "response").await;
        }
        DispatchResult::Stream { mut frames, done } => {
            while let Some(frame) = frames.recv().await {
                if !send_body(&write_tx, &frame, "stream frame").await {
                    span.record(observability::attr::OUTCOME, "error");
                    span.record(observability::attr::ERROR_KIND, "client_disconnected");
                    return;
                }
            }
            match done.await {
                Ok(resp) => {
                    record_response_outcome(span, &resp);
                    send_body(&write_tx, &resp, "final response").await;
                }
                Err(_) => {
                    span.record(observability::attr::OUTCOME, "error");
                    span.record(observability::attr::ERROR_KIND, "stream_done_dropped");
                    tracing::warn!("daemon: stream completion sender dropped without result");
                }
            }
        }
    }
}

fn record_response_outcome(span: &tracing::Span, response: &Response) {
    if let Some(error) = &response.error {
        span.record(observability::attr::OUTCOME, "error");
        span.record(observability::attr::ERROR_KIND, error.code.as_str());
    } else {
        span.record(observability::attr::OUTCOME, "ok");
    }
}

async fn send_body<T: serde::Serialize>(
    write_tx: &mpsc::Sender<Vec<u8>>,
    value: &T,
    label: &str,
) -> bool {
    let body = match serde_json::to_vec(value) {
        Ok(body) => body,
        Err(e) => {
            tracing::warn!(error = %e, "daemon: failed to serialize {label}");
            return true;
        }
    };
    write_tx.send(body).await.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::protocol::*;
    use crate::daemon::state::DaemonState;
    use std::sync::Arc;
    use tempfile::tempdir;
    use tokio::net::UnixStream;

    async fn ping(stream: &mut UnixStream, id: &str) -> Response {
        let req = Request {
            version: 1,
            id: id.into(),
            session: "main".into(),
            method: "daemon.ping".into(),
            params: serde_json::json!({}),
        };
        write_request(stream, &req).await.unwrap();
        let body = read_frame_bytes(stream).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn server_responds_to_ping() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("vulcan.sock");
        let state = Arc::new(DaemonState::for_tests_minimal());
        let server = Server::bind(&path, state.clone()).await.unwrap();
        let handle = tokio::spawn(server.run());

        let mut client = UnixStream::connect(&path).await.unwrap();
        let resp = ping(&mut client, "p1").await;
        assert_eq!(resp.id, "p1");
        assert_eq!(resp.result.unwrap()["pong"], true);

        state.signal_shutdown();
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("server stops")
            .unwrap();
    }

    #[tokio::test]
    async fn server_handles_concurrent_clients() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("vulcan.sock");
        let state = Arc::new(DaemonState::for_tests_minimal());
        let server = Server::bind(&path, state.clone()).await.unwrap();
        let handle = tokio::spawn(server.run());

        let mut clients = vec![];
        for i in 0..8 {
            let p = path.clone();
            clients.push(tokio::spawn(async move {
                let mut s = UnixStream::connect(&p).await.unwrap();
                let req = Request {
                    version: 1,
                    id: format!("c{i}"),
                    session: "main".into(),
                    method: "daemon.ping".into(),
                    params: serde_json::json!({}),
                };
                write_request(&mut s, &req).await.unwrap();
                let body = read_frame_bytes(&mut s).await.unwrap();
                let resp: Response = serde_json::from_slice(&body).unwrap();
                assert_eq!(resp.result.unwrap()["pong"], true);
            }));
        }
        for c in clients {
            c.await.unwrap();
        }

        state.signal_shutdown();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn server_keeps_connection_alive_for_multiple_requests() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("vulcan.sock");
        let state = Arc::new(DaemonState::for_tests_minimal());
        let server = Server::bind(&path, state.clone()).await.unwrap();
        let handle = tokio::spawn(server.run());

        let mut client = UnixStream::connect(&path).await.unwrap();
        let r1 = ping(&mut client, "r1").await;
        assert_eq!(r1.id, "r1");
        let r2 = ping(&mut client, "r2").await;
        assert_eq!(r2.id, "r2");
        let r3 = ping(&mut client, "r3").await;
        assert_eq!(r3.id, "r3");

        state.signal_shutdown();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn server_reads_next_request_while_stream_is_in_flight() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("vulcan.sock");
        let state = Arc::new(DaemonState::for_tests_minimal());
        let server = Server::bind(&path, state.clone()).await.unwrap();
        let handle = tokio::spawn(server.run());

        let mut client = UnixStream::connect(&path).await.unwrap();
        let stream_req = Request {
            version: 1,
            id: "stream-1".into(),
            session: "main".into(),
            method: "test.slow_stream".into(),
            params: serde_json::json!({}),
        };
        let ping_req = Request {
            version: 1,
            id: "ping-while-streaming".into(),
            session: "main".into(),
            method: "daemon.ping".into(),
            params: serde_json::json!({}),
        };

        write_request(&mut client, &stream_req).await.unwrap();
        write_request(&mut client, &ping_req).await.unwrap();

        let body = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            read_frame_bytes(&mut client),
        )
        .await
        .expect("ping should not wait for slow stream")
        .unwrap();
        let resp: Response = serde_json::from_slice(&body).expect("first frame should be response");
        assert_eq!(resp.id, "ping-while-streaming");
        assert_eq!(resp.result.unwrap()["pong"], true);

        state.signal_shutdown();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn server_returns_version_mismatch_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("vulcan.sock");
        let state = Arc::new(DaemonState::for_tests_minimal());
        let server = Server::bind(&path, state.clone()).await.unwrap();
        let handle = tokio::spawn(server.run());

        let mut client = UnixStream::connect(&path).await.unwrap();
        let bad = serde_json::json!({
            "version": 99, "id": "v1", "session": "main",
            "method": "daemon.ping", "params": {}
        });
        let body = serde_json::to_vec(&bad).unwrap();
        write_frame_bytes(&mut client, &body).await.unwrap();

        let body = read_frame_bytes(&mut client).await.unwrap();
        let resp: Response = serde_json::from_slice(&body).unwrap();
        let err = resp.error.expect("server returns structured error");
        assert_eq!(
            err.code, "VERSION_MISMATCH",
            "structured error preserved across socket boundary"
        );

        state.signal_shutdown();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn server_returns_unknown_method_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("vulcan.sock");
        let state = Arc::new(DaemonState::for_tests_minimal());
        let server = Server::bind(&path, state.clone()).await.unwrap();
        let handle = tokio::spawn(server.run());

        let mut client = UnixStream::connect(&path).await.unwrap();
        let req = Request {
            version: 1,
            id: "u1".into(),
            session: "main".into(),
            method: "method.does.not.exist".into(),
            params: serde_json::json!({}),
        };
        write_request(&mut client, &req).await.unwrap();
        let body = read_frame_bytes(&mut client).await.unwrap();
        let resp: Response = serde_json::from_slice(&body).unwrap();
        let err = resp
            .error
            .expect("dispatcher error survives socket boundary");
        assert_eq!(err.code, "UNKNOWN_METHOD");
        assert!(err.message.contains("method.does.not.exist"));

        state.signal_shutdown();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }

    #[tokio::test]
    async fn server_shuts_down_on_signal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("vulcan.sock");
        let state = Arc::new(DaemonState::for_tests_minimal());
        let server = Server::bind(&path, state.clone()).await.unwrap();
        let handle = tokio::spawn(server.run());

        // Give server a moment to settle into accept().
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        state.signal_shutdown();
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("must shut down within 2s")
            .unwrap();
    }

    #[tokio::test]
    async fn server_drops_connection_on_eof() {
        // Client closes; server's per-conn task should exit cleanly without panic.
        let dir = tempdir().unwrap();
        let path = dir.path().join("vulcan.sock");
        let state = Arc::new(DaemonState::for_tests_minimal());
        let server = Server::bind(&path, state.clone()).await.unwrap();
        let handle = tokio::spawn(server.run());

        {
            let _client = UnixStream::connect(&path).await.unwrap();
            // immediately drop (EOF)
        }
        // No assertion needed — the test passes if the server doesn't panic.
        // Give the server a moment to handle the EOF.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        state.signal_shutdown();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
    }
}
