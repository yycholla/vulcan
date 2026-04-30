//! Maps a gateway [`LaneKey`] (platform + chat_id) to a daemon session
//! id.
//!
//! This replaces the per-lane in-process Agent cache from earlier
//! slices (Slice 3 Task 3.4). The daemon owns the Agent — one per
//! session, lazy-built on first `prompt.run` against that session —
//! and the gateway becomes a thin Axum + queue front-end.
//!
//! Session id format: `"gateway:{platform}:{chat_id}"`. Stable per
//! lane so reconnects map to the same daemon session — and so the
//! daemon-side idle-eviction TTL lives between gateway processes.
//!
//! Naming note: `crate::gateway::lane::LaneRouter<M>` already exists
//! as a generic per-key serial dispatcher (one mpsc worker per
//! `LaneKey`). To avoid a name collision we expose this struct as
//! `DaemonLaneRouter`. The two play distinct roles and live in
//! distinct modules.

use std::collections::HashMap;

use parking_lot::Mutex;

use crate::client::{Client, ClientError};
use crate::gateway::lane::LaneKey;

#[derive(Debug, thiserror::Error)]
pub enum LaneRouterError {
    /// Underlying daemon RPC (auto-start, transport, dispatch) failed.
    #[error("daemon RPC failed: {0}")]
    Rpc(String),
    /// The daemon answered but with an unrecognized payload shape.
    #[error("session.create returned malformed response: {0}")]
    BadResponse(String),
}

impl From<ClientError> for LaneRouterError {
    fn from(err: ClientError) -> Self {
        LaneRouterError::Rpc(err.to_string())
    }
}

/// Owns the lane → session-id mapping. `ensure_session` is
/// idempotent; the daemon's `session.create` rejects duplicate ids so
/// the cache also serves as a write-through guard.
pub struct DaemonLaneRouter {
    sessions: Mutex<HashMap<LaneKey, String>>,
}

impl DaemonLaneRouter {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Format a [`LaneKey`] into a stable daemon session id. Public so
    /// callers (e.g. /v1/lanes) can derive the same id without going
    /// through `ensure_session`.
    pub fn derive_session_id(lane: &LaneKey) -> String {
        format!("gateway:{}:{}", lane.platform, lane.chat_id)
    }

    /// Get (or create) the daemon session id for a lane. Idempotent:
    /// a `SESSION_EXISTS` reply from the daemon is treated as success
    /// so reconnects across gateway restarts are no-ops.
    pub async fn ensure_session(
        &self,
        lane: &LaneKey,
        client: &Client,
    ) -> Result<String, LaneRouterError> {
        // Cache hit: skip the RPC entirely.
        if let Some(sid) = self.sessions.lock().get(lane).cloned() {
            return Ok(sid);
        }

        let sid = Self::derive_session_id(lane);

        // session.create with `id` set: re-creating an existing
        // session returns SESSION_EXISTS, which we treat as success
        // here (lane mapping is idempotent across gateway restarts).
        match client
            .call("session.create", serde_json::json!({ "id": sid.clone() }))
            .await
        {
            Ok(_) => {}
            Err(ClientError::Daemon(err)) if err.code == "SESSION_EXISTS" => {}
            Err(e) => return Err(e.into()),
        }

        self.sessions.lock().insert(lane.clone(), sid.clone());
        Ok(sid)
    }

    /// Number of cached lanes (surface for /v1/lanes route).
    pub fn cached_lane_count(&self) -> usize {
        self.sessions.lock().len()
    }

    /// Remove the cache entry for this lane. The next call to
    /// `ensure_session` for this lane will go through the full
    /// `session.create` round-trip. Used after `/clear` (which
    /// destroys the daemon session) to keep the cache coherent —
    /// without this, the stale session id would be reused and the
    /// next `prompt.stream` would fail with `SESSION_NOT_FOUND`.
    pub fn forget(&self, lane: &LaneKey) {
        self.sessions.lock().remove(lane);
    }

    /// Snapshot of the lane → session-id mapping for diagnostics.
    /// Sorted by lane for stable JSON output. Returns owned strings so
    /// callers can serialize without holding the lock.
    pub fn snapshot_cache(&self) -> Vec<LaneCacheEntry> {
        let g = self.sessions.lock();
        let mut out: Vec<LaneCacheEntry> = g
            .iter()
            .map(|(k, v)| LaneCacheEntry {
                platform: k.platform.clone(),
                chat_id: k.chat_id.clone(),
                session_id: v.clone(),
            })
            .collect();
        out.sort_by(|a, b| {
            a.platform
                .cmp(&b.platform)
                .then_with(|| a.chat_id.cmp(&b.chat_id))
        });
        out
    }
}

impl Default for DaemonLaneRouter {
    fn default() -> Self {
        Self::new()
    }
}

/// One entry in the diagnostic snapshot returned by
/// [`DaemonLaneRouter::snapshot_cache`]. Surfaced through `GET /v1/lanes`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LaneCacheEntry {
    pub platform: String,
    pub chat_id: String,
    pub session_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::server::Server;
    use crate::daemon::state::DaemonState;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::tempdir;

    fn lane(p: &str, c: &str) -> LaneKey {
        LaneKey {
            platform: p.into(),
            chat_id: c.into(),
        }
    }

    #[test]
    fn derive_session_id_is_stable() {
        let l = lane("discord", "12345");
        assert_eq!(
            DaemonLaneRouter::derive_session_id(&l),
            "gateway:discord:12345"
        );
        // Same input → same output across calls (no hidden RNG).
        assert_eq!(
            DaemonLaneRouter::derive_session_id(&l),
            DaemonLaneRouter::derive_session_id(&l),
        );
    }

    /// End-to-end against a real (tempdir) daemon: `ensure_session`
    /// creates and caches per-lane session ids, distinct lanes get
    /// distinct sessions, and a second call against the same lane
    /// hits the cache.
    #[tokio::test]
    async fn ensure_session_creates_and_caches() {
        let dir = tempdir().unwrap();
        let sock = dir.path().join("vulcan.sock");

        let state = Arc::new(DaemonState::for_tests_minimal());
        let server = Server::bind(&sock, state.clone()).await.unwrap();
        let server_handle = tokio::spawn(server.run());

        // Wait briefly for the listener to settle.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let router = DaemonLaneRouter::new();
        let client = Client::connect_at(&sock).await.unwrap();

        let l1 = lane("discord", "111");
        let l2 = lane("discord", "222");

        let s1 = router.ensure_session(&l1, &client).await.unwrap();
        assert_eq!(s1, "gateway:discord:111");

        let s2 = router.ensure_session(&l2, &client).await.unwrap();
        assert_eq!(s2, "gateway:discord:222");
        assert_ne!(s1, s2, "distinct lanes must map to distinct sessions");

        // Second call same lane: cache hit. Result must equal first.
        let s1_again = router.ensure_session(&l1, &client).await.unwrap();
        assert_eq!(s1_again, s1);

        assert_eq!(router.cached_lane_count(), 2);
        let snap = router.snapshot_cache();
        assert_eq!(snap.len(), 2);
        // Snapshot is sorted by (platform, chat_id) so 111 < 222.
        assert_eq!(snap[0].chat_id, "111");
        assert_eq!(snap[0].session_id, "gateway:discord:111");
        assert_eq!(snap[1].chat_id, "222");

        state.signal_shutdown();
        let _ = tokio::time::timeout(Duration::from_secs(2), server_handle).await;
    }

    /// `forget` clears the cached lane → session-id entry so the next
    /// `ensure_session` goes through `session.create` again. Mirrors
    /// the path taken after `/clear` destroys the daemon session.
    #[tokio::test]
    async fn forget_clears_cache_entry() {
        let dir = tempdir().unwrap();
        let sock = dir.path().join("vulcan.sock");

        let state = Arc::new(DaemonState::for_tests_minimal());
        let server = Server::bind(&sock, state.clone()).await.unwrap();
        let server_handle = tokio::spawn(server.run());
        tokio::time::sleep(Duration::from_millis(50)).await;

        let router = DaemonLaneRouter::new();
        let client = Client::connect_at(&sock).await.unwrap();

        let l = lane("discord", "111");
        let s1 = router.ensure_session(&l, &client).await.unwrap();
        assert_eq!(router.cached_lane_count(), 1);

        router.forget(&l);
        assert_eq!(router.cached_lane_count(), 0, "forget removes cache entry",);

        // Re-ensure works (idempotent SESSION_EXISTS handling on the
        // daemon side: the session itself was never destroyed in this
        // test, only the gateway's cache was invalidated, so the
        // re-create round-trips through SESSION_EXISTS).
        let s2 = router.ensure_session(&l, &client).await.unwrap();
        assert_eq!(s1, s2, "same session id after forget+re-ensure");
        assert_eq!(router.cached_lane_count(), 1);

        state.signal_shutdown();
        let _ = tokio::time::timeout(Duration::from_secs(2), server_handle).await;
    }

    /// `ensure_session` must be idempotent against a daemon that
    /// already has the session: re-creating an existing session
    /// returns SESSION_EXISTS, which the router treats as success.
    #[tokio::test]
    async fn ensure_session_treats_existing_session_as_success() {
        let dir = tempdir().unwrap();
        let sock = dir.path().join("vulcan.sock");

        let state = Arc::new(DaemonState::for_tests_minimal());
        let server = Server::bind(&sock, state.clone()).await.unwrap();
        let server_handle = tokio::spawn(server.run());
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Pre-create the session out-of-band so the router's own
        // session.create gets SESSION_EXISTS.
        let pre_sid = "gateway:loopback:42";
        {
            let c = Client::connect_at(&sock).await.unwrap();
            let _ = c
                .call("session.create", serde_json::json!({ "id": pre_sid }))
                .await
                .unwrap();
        }

        let router = DaemonLaneRouter::new();
        let client = Client::connect_at(&sock).await.unwrap();

        let l = lane("loopback", "42");
        let sid = router
            .ensure_session(&l, &client)
            .await
            .expect("SESSION_EXISTS must be treated as success");
        assert_eq!(sid, pre_sid);

        state.signal_shutdown();
        let _ = tokio::time::timeout(Duration::from_secs(2), server_handle).await;
    }
}
