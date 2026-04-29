//! Session map and per-session state.
//!
//! Phase 3: each `SessionState` owns an optional warm Agent. The "main"
//! session is pre-created on daemon boot (without an Agent), then the
//! daemon startup path builds the Agent and installs it via
//! [`SessionState::set_agent`]. Additional sessions can be created
//! on-demand via `session.create`; their Agents are built lazily.
//!
//! The `"main"` session cannot be destroyed via
//! [`SessionMap::destroy_checked`] — it's the implicit default when a
//! request envelope omits or sends `"main"` for the `session` field.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

/// Shared, async-locked handle to a per-session Agent. `Arc` so a
/// single session can be locked from concurrent tasks (e.g. a
/// streaming `prompt.stream` background task and an inbound
/// `prompt.cancel` on the same session).
pub type AgentHandle = Arc<tokio::sync::Mutex<crate::agent::Agent>>;

/// Per-session state.
pub struct SessionState {
    pub id: String,
    pub created_at: Instant,
    pub last_activity: Mutex<Instant>,
    pub in_flight: Mutex<bool>,
    pub cancel: CancellationToken,
    /// Phase 3: each session optionally owns its own warm Agent.
    /// `None` until the Agent is built (main at boot, others on
    /// create). Stored as `Arc<AsyncMutex<Agent>>` so callers can
    /// cheaply clone a handle and lock the Agent across `await`
    /// points without holding the outer `parking_lot::Mutex`.
    pub agent: Mutex<Option<AgentHandle>>,
}

impl SessionState {
    pub fn new(id: String) -> Self {
        let now = Instant::now();
        Self {
            id,
            created_at: now,
            last_activity: Mutex::new(now),
            in_flight: Mutex::new(false),
            cancel: CancellationToken::new(),
            agent: Mutex::new(None),
        }
    }

    /// Install a warm Agent into this session. Called by daemon startup
    /// for the "main" session, and by `session.create` for new sessions.
    pub fn set_agent(&self, agent: crate::agent::Agent) {
        *self.agent.lock() = Some(Arc::new(tokio::sync::Mutex::new(agent)));
    }

    /// Update `last_activity` to `Instant::now()`. Call when this
    /// session services any RPC; idle eviction reads this.
    pub fn touch(&self) {
        *self.last_activity.lock() = Instant::now();
    }

    /// True if this session has a warm Agent installed.
    pub fn has_agent(&self) -> bool {
        self.agent.lock().is_some()
    }

    /// Cloneable handle to the per-session Agent, if installed.
    /// Returns `None` if the session has no Agent yet (e.g. created
    /// via `session.create` for non-main; lazy-build is deferred).
    pub fn agent_arc(&self) -> Option<AgentHandle> {
        self.agent.lock().clone()
    }
}

/// Concurrent map of session id → state. Cheap reads under
/// `parking_lot::RwLock`; writes are infrequent (create/destroy).
pub struct SessionMap {
    inner: RwLock<HashMap<String, Arc<SessionState>>>,
}

impl SessionMap {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// Build a map with the default `"main"` session pre-created.
    pub fn with_main() -> Self {
        let map = Self::new();
        map.inner
            .write()
            .insert("main".into(), Arc::new(SessionState::new("main".into())));
        map
    }

    pub fn get(&self, id: &str) -> Option<Arc<SessionState>> {
        self.inner.read().get(id).cloned()
    }

    /// Insert a new session with the given id. Errors if the id is
    /// already present. Returns the id on success (convenience for
    /// chaining).
    pub fn create_named(&self, id: &str) -> anyhow::Result<String> {
        let mut g = self.inner.write();
        if g.contains_key(id) {
            anyhow::bail!("session already exists: {id}");
        }
        g.insert(id.into(), Arc::new(SessionState::new(id.into())));
        Ok(id.into())
    }

    /// Remove a session unconditionally. Use [`Self::destroy_checked`] in
    /// production to guard `"main"`.
    pub fn destroy(&self, id: &str) {
        self.inner.write().remove(id);
    }

    /// Remove a session, refusing to destroy `"main"`.
    pub fn destroy_checked(&self, id: &str) -> anyhow::Result<()> {
        if id == "main" {
            anyhow::bail!("cannot destroy 'main' session");
        }
        self.destroy(id);
        Ok(())
    }

    /// Status snapshot for `daemon.status`. Each entry is a JSON
    /// object with id / in_flight / last_activity_secs_ago.
    pub fn descriptors(&self) -> Vec<serde_json::Value> {
        let g = self.inner.read();
        g.values()
            .map(|s| {
                let last = s.last_activity.lock();
                serde_json::json!({
                    "id": s.id,
                    "in_flight": *s.in_flight.lock(),
                    "last_activity_secs_ago": last.elapsed().as_secs(),
                    "has_agent": s.has_agent(),
                })
            })
            .collect()
    }

    /// True if any session has `in_flight == true`. Used by Task 0.10's
    /// config-watch loop to defer reload until idle.
    pub fn any_in_flight(&self) -> bool {
        let g = self.inner.read();
        g.values().any(|s| *s.in_flight.lock())
    }

    /// Count of sessions currently alive.
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// True if the map is empty. (Practically always false in
    /// production: `with_main` seeds the `"main"` session.)
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    /// List all session ids.
    pub fn ids(&self) -> Vec<String> {
        self.inner.read().keys().cloned().collect()
    }
}

impl Default for SessionMap {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_with_main_has_main_session() {
        let map = SessionMap::with_main();
        assert!(map.get("main").is_some());
        assert!(map.get("nonexistent").is_none());
    }

    #[test]
    fn create_named_inserts_session() {
        let map = SessionMap::with_main();
        let id = map.create_named("foo").unwrap();
        assert_eq!(id, "foo");
        assert!(map.get("foo").is_some());
    }

    #[test]
    fn create_named_rejects_duplicate() {
        let map = SessionMap::with_main();
        map.create_named("foo").unwrap();
        let err = map.create_named("foo").expect_err("must reject duplicate");
        assert!(err.to_string().contains("foo"), "error mentions session id");
    }

    #[test]
    fn destroy_removes_session() {
        let map = SessionMap::with_main();
        map.create_named("foo").unwrap();
        map.destroy("foo");
        assert!(map.get("foo").is_none());
    }

    #[test]
    fn destroy_checked_rejects_main() {
        let map = SessionMap::with_main();
        let err = map
            .destroy_checked("main")
            .expect_err("main is undeletable");
        assert!(err.to_string().contains("main"));
        assert!(map.get("main").is_some(), "main still present");
    }

    #[test]
    fn destroy_checked_allows_others() {
        let map = SessionMap::with_main();
        map.create_named("foo").unwrap();
        map.destroy_checked("foo").unwrap();
        assert!(map.get("foo").is_none());
    }

    #[test]
    fn descriptors_includes_main() {
        let map = SessionMap::with_main();
        let d = map.descriptors();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0]["id"], "main");
    }

    #[test]
    fn any_in_flight_false_when_idle() {
        let map = SessionMap::with_main();
        assert!(!map.any_in_flight());
    }

    #[test]
    fn any_in_flight_true_when_session_busy() {
        let map = SessionMap::with_main();
        *map.get("main").unwrap().in_flight.lock() = true;
        assert!(map.any_in_flight());
    }

    #[test]
    fn len_counts_correctly() {
        let map = SessionMap::with_main();
        assert_eq!(map.len(), 1);
        map.create_named("foo").unwrap();
        assert_eq!(map.len(), 2);
    }
}
