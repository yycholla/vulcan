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

use crate::config::vulcan_home;
use crate::daemon::protocol::Request;

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
    pub async fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> ClientResult<serde_json::Value> {
        let req = Request {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            session: "main".into(),
            method: method.into(),
            params,
        };
        let resp = self.transport.call(req).await?;
        if let Some(err) = resp.error {
            return Err(err.into());
        }
        Ok(resp.result.unwrap_or_default())
    }
}
