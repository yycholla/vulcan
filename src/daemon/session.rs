//! Session map and per-session state.
//!
//! In Slice 0 each `SessionState` is a small marker holding an id,
//! activity timestamps, an in-flight flag, and a cancellation token.
//! Slice 2 (full Agent in daemon) will add the heavy fields: `Agent`,
//! audit buffer, etc. Slice 3 will add idle-eviction policy.
//!
//! The `"main"` session is pre-created on daemon boot and cannot be
//! destroyed via [`SessionMap::destroy_checked`] — it's the implicit
//! default when a request envelope omits or sends `"main"` for the
//! `session` field.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

/// Per-session state. Slice 0 fields only; Slice 2 will add `Agent` etc.
pub struct SessionState {
    pub id: String,
    pub created_at: Instant,
    pub last_activity: Mutex<Instant>,
    pub in_flight: Mutex<bool>,
    pub cancel: CancellationToken,
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
        }
    }

    /// Update `last_activity` to `Instant::now()`. Call when this
    /// session services any RPC; idle eviction (Slice 3) reads this.
    pub fn touch(&self) {
        *self.last_activity.lock() = Instant::now();
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
    fn descriptors_serializes_each_session() {
        let map = SessionMap::with_main();
        map.create_named("foo").unwrap();
        let desc = map.descriptors();
        assert_eq!(desc.len(), 2, "main + foo");
        // Each descriptor must have id, in_flight, last_activity_secs_ago
        for d in &desc {
            assert!(d["id"].is_string());
            assert!(d["in_flight"].is_boolean());
            assert!(d["last_activity_secs_ago"].is_number());
        }
        // ids should include both "main" and "foo" (order not guaranteed)
        let ids: Vec<String> = desc
            .iter()
            .map(|d| d["id"].as_str().unwrap().to_string())
            .collect();
        assert!(ids.contains(&"main".to_string()));
        assert!(ids.contains(&"foo".to_string()));
    }

    #[test]
    fn any_in_flight_reflects_session_state() {
        let map = SessionMap::with_main();
        assert!(!map.any_in_flight(), "fresh map: no in-flight");

        let main = map.get("main").unwrap();
        *main.in_flight.lock() = true;
        assert!(map.any_in_flight());

        *main.in_flight.lock() = false;
        assert!(!map.any_in_flight());
    }

    #[test]
    fn touch_updates_last_activity() {
        let map = SessionMap::with_main();
        let main = map.get("main").unwrap();
        let initial = *main.last_activity.lock();
        std::thread::sleep(std::time::Duration::from_millis(10));
        main.touch();
        let after = *main.last_activity.lock();
        assert!(after > initial, "touch advances last_activity");
    }
}
