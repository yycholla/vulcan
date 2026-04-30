//! In-tree client for the `vulcan` daemon (YYC-266 Slice 0 Task 0.11).
//!
//! Frontend code (CLI subcommands, future TUI/gateway adapters) constructs
//! a [`Client`] via [`Client::connect_or_autostart`], which transparently
//! spawns a daemon process if none is running. The [`Client::call`] method
//! is the primary RPC entry point; it returns either a typed daemon
//! response or a [`ClientError`].
//!
//! The client lives in-tree (not as a separate crate) deliberately:
//! single binary, no other consumer, no dependency-graph isolation
//! benefit. If a third-party embedder ever needs the protocol bindings,
//! we'll extract `vulcan-protocol` first and re-export from here.

mod auto_start;
mod errors;
mod transport;

pub use errors::{ClientError, ClientResult};
pub use transport::StreamFrames;

use crate::config::vulcan_home;
use crate::daemon::protocol::{Request, StreamFrame};

use transport::Transport;

pub struct Client {
    transport: Transport,
}

impl Client {
    /// Connect to the daemon, auto-starting one if no socket is reachable.
    /// The socket path is derived from [`vulcan_home`] (i.e. `$VULCAN_HOME`
    /// or `$HOME/.vulcan`).
    pub async fn connect_or_autostart() -> ClientResult<Self> {
        let sock = vulcan_home().join("vulcan.sock");
        auto_start::ensure_daemon(&sock).await?;
        let transport = Transport::connect(&sock).await?;
        Ok(Self { transport })
    }

    /// Connect to an existing daemon at `sock`. Does NOT auto-start —
    /// use this from contexts that already manage daemon lifecycle
    /// (tests, recovery paths) and want a hard error if no daemon is
    /// listening.
    pub async fn connect_at(sock: &std::path::Path) -> ClientResult<Self> {
        let transport = Transport::connect(sock).await?;
        Ok(Self { transport })
    }

    /// Make a single RPC call. The session defaults to `"main"`. The
    /// request id is a fresh UUIDv4 so that responses on the same
    /// connection cannot be confused with cached/retried prior frames.
    ///
    /// Slice 5: takes `&self` so a single Client can serve multiple
    /// concurrent in-flight calls — the underlying transport's reader
    /// task demultiplexes responses by request id.
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> ClientResult<serde_json::Value> {
        self.call_at_session("main", method, params).await
    }

    /// Like [`Self::call`] but routes the request to an explicit
    /// session id rather than the default `"main"`. Used by gateway /
    /// orchestrator code that maps external lanes to per-session
    /// daemon Agents.
    pub async fn call_at_session(
        &self,
        session: &str,
        method: &str,
        params: serde_json::Value,
    ) -> ClientResult<serde_json::Value> {
        let req = Request {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            session: session.into(),
            method: method.into(),
            params,
        };
        let resp = self.transport.call(req).await?;
        if let Some(err) = resp.error {
            return Err(err.into());
        }
        Ok(resp.result.unwrap_or_default())
    }

    /// Initiate a streaming RPC call against the default `"main"`
    /// session. Returns a [`StreamFrames`] handle whose `frames`
    /// receiver yields incremental [`crate::daemon::protocol::StreamFrame`]s
    /// and whose `done` oneshot resolves to the final
    /// [`crate::daemon::protocol::Response`].
    #[allow(dead_code)]
    pub async fn call_stream(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> ClientResult<StreamFrames> {
        self.call_stream_at_session("main", method, params).await
    }

    /// Like [`Self::call_stream`] but targets an explicit session id.
    pub async fn call_stream_at_session(
        &self,
        session: &str,
        method: &str,
        params: serde_json::Value,
    ) -> ClientResult<StreamFrames> {
        let req = Request {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            session: session.into(),
            method: method.into(),
            params,
        };
        self.transport.call_stream(req).await
    }

    /// Take the daemon push-frame receiver for this client. Returns
    /// `None` if another consumer already took it.
    pub async fn take_push_receiver(&self) -> Option<tokio::sync::mpsc::Receiver<StreamFrame>> {
        self.transport.take_push_receiver().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::protocol::{
        Response, StreamFrame, read_frame_bytes, write_frame_bytes, write_response,
    };
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::net::UnixListener;

    /// Bind a UnixListener at a temp path and spawn a fake daemon that
    /// answers `daemon.ping`-style calls with `{ "echo": id }`. The
    /// answers are written in **reverse arrival order** to prove that
    /// the client's id-routing demultiplexes correctly: a naive
    /// FIFO read loop would route the second response to the first
    /// caller and fail the assertion.
    async fn spawn_reordering_echo_daemon(tmp: &TempDir) -> std::path::PathBuf {
        let sock = tmp.path().join("test.sock");
        let listener = UnixListener::bind(&sock).expect("bind test sock");
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (mut read, mut write) = stream.into_split();
            // Read two requests, then answer them in reversed order.
            let body1 = read_frame_bytes(&mut read).await.expect("frame1");
            let body2 = read_frame_bytes(&mut read).await.expect("frame2");
            let req1: Request = serde_json::from_slice(&body1).expect("decode req1");
            let req2: Request = serde_json::from_slice(&body2).expect("decode req2");
            // Stall a touch so both calls are definitely in-flight.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let resp2 = Response::ok(req2.id.clone(), serde_json::json!({"echo": req2.id}));
            let resp1 = Response::ok(req1.id.clone(), serde_json::json!({"echo": req1.id}));
            write_response(&mut write, &resp2).await.expect("write2");
            write_response(&mut write, &resp1).await.expect("write1");
            // Keep the connection open so the client's reader doesn't
            // close before we finish the test.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        });
        sock
    }

    #[tokio::test]
    async fn client_handles_concurrent_calls_via_id_routing() {
        // Slice 5 acceptance: one Client serves multiple in-flight
        // calls; responses are routed by request id, not arrival
        // order. The fake daemon writes responses in reverse, so a
        // FIFO transport would mis-route them and fail.
        let tmp = TempDir::new().expect("tempdir");
        let sock = spawn_reordering_echo_daemon(&tmp).await;
        // Give the listener a tick to be ready.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let client = Arc::new(Client::connect_at(&sock).await.expect("connect"));

        let c1 = Arc::clone(&client);
        let c2 = Arc::clone(&client);
        let h1 = tokio::spawn(async move {
            c1.call("ping1", serde_json::json!({}))
                .await
                .expect("call1")
        });
        let h2 = tokio::spawn(async move {
            c2.call("ping2", serde_json::json!({}))
                .await
                .expect("call2")
        });
        let r1 = h1.await.expect("join1");
        let r2 = h2.await.expect("join2");

        // Each call's response must echo its own id.
        let echo1 = r1.get("echo").and_then(|v| v.as_str()).unwrap();
        let echo2 = r2.get("echo").and_then(|v| v.as_str()).unwrap();
        assert_ne!(echo1, echo2, "different request ids must echo different");
    }

    async fn spawn_streaming_echo_daemon(tmp: &TempDir) -> std::path::PathBuf {
        let sock = tmp.path().join("stream.sock");
        let listener = UnixListener::bind(&sock).expect("bind test sock");
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let (mut read, mut write) = stream.into_split();
            let body1 = read_frame_bytes(&mut read).await.expect("frame1");
            let body2 = read_frame_bytes(&mut read).await.expect("frame2");
            let req1: Request = serde_json::from_slice(&body1).expect("decode req1");
            let req2: Request = serde_json::from_slice(&body2).expect("decode req2");
            let (stream_req, call_req) = if req1.method == "prompt.stream" {
                (req1, req2)
            } else {
                (req2, req1)
            };

            let ping = Response::ok(call_req.id.clone(), serde_json::json!({"pong": true}));
            write_response(&mut write, &ping).await.expect("write ping");

            let frame = StreamFrame {
                version: 1,
                id: Some(stream_req.id.clone()),
                stream: "text".into(),
                data: serde_json::json!({"text": "hello"}),
            };
            let body = serde_json::to_vec(&frame).unwrap();
            write_frame_bytes(&mut write, &body)
                .await
                .expect("write frame");
            let done = Response::ok(stream_req.id, serde_json::json!({"ok": true}));
            write_response(&mut write, &done).await.expect("write done");

            let push = StreamFrame {
                version: 1,
                id: None,
                stream: "config_reloaded".into(),
                data: serde_json::json!({"reloads": 1}),
            };
            let body = serde_json::to_vec(&push).unwrap();
            write_frame_bytes(&mut write, &body)
                .await
                .expect("write push");
        });
        sock
    }

    #[tokio::test]
    async fn client_routes_stream_frames_calls_and_pushes_on_one_socket() {
        let tmp = TempDir::new().expect("tempdir");
        let sock = spawn_streaming_echo_daemon(&tmp).await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let client = Client::connect_at(&sock).await.expect("connect");
        let mut pushes = client
            .take_push_receiver()
            .await
            .expect("push receiver available");

        let mut stream = client
            .call_stream("prompt.stream", serde_json::json!({"text": "hi"}))
            .await
            .expect("stream call starts");
        let ping = client
            .call("daemon.ping", serde_json::json!({}))
            .await
            .expect("normal call can share socket");
        assert_eq!(ping["pong"], true);

        let frame = stream.frames.recv().await.expect("stream frame");
        assert_eq!(frame.stream, "text");
        assert_eq!(frame.data["text"], "hello");
        let done = stream
            .done
            .await
            .expect("done channel")
            .expect("done response");
        assert!(done.error.is_none());
        assert_eq!(done.result.unwrap()["ok"], true);

        let push = pushes.recv().await.expect("push frame");
        assert_eq!(push.id, None);
        assert_eq!(push.stream, "config_reloaded");
        assert_eq!(push.data["reloads"], 1);
    }
}
