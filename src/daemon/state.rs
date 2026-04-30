//! Long-lived daemon process state. Holds the shutdown / reload signals,
//! the SessionMap, and the shared CortexStore (Slice 1).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::{Notify, watch};

use crate::config::Config;
use crate::daemon::session::SessionMap;
use crate::memory::cortex::CortexStore;
use crate::runtime_pool::RuntimeResourcePool;

/// Per-process daemon state, shared across all connections.
pub struct DaemonState {
    started_at: Instant,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    reload: Arc<Notify>,
    sessions: Arc<SessionMap>,
    reloads_applied: AtomicU64,
    cortex: Option<Arc<CortexStore>>,
    /// Slice 3: daemon-owned shared adapters (session store, run
    /// store, artifact store, orchestration). `Option` so existing
    /// minimal-test constructors don't pay the SQLite-open cost; the
    /// production `with_pool` builder installs a real pool.
    pool: Option<Arc<RuntimeResourcePool>>,
    /// Config snapshot loaded at daemon boot. Lazy-build paths in
    /// `SessionState::ensure_agent` reference this so handlers don't
    /// have to re-load from disk.
    config: Arc<Config>,
}

impl DaemonState {
    pub fn new(config: Arc<Config>) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            started_at: Instant::now(),
            shutdown_tx,
            shutdown_rx,
            reload: Arc::new(Notify::new()),
            sessions: Arc::new(SessionMap::with_main()),
            reloads_applied: AtomicU64::new(0),
            cortex: None,
            pool: None,
            config,
        }
    }

    /// Slice 3: install the daemon-owned [`RuntimeResourcePool`].
    /// Called by the daemon startup path after opening the pool.
    pub fn with_pool(mut self, pool: Arc<RuntimeResourcePool>) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Borrow the daemon-owned runtime resource pool, if installed.
    pub fn pool(&self) -> Option<&Arc<RuntimeResourcePool>> {
        self.pool.as_ref()
    }

    /// Test-only constructor. Returns a `DaemonState` with the default
    /// `"main"` session pre-created and no Agent/Cortex installed —
    /// matching the post-boot, pre-warm-build state. Tests that need a
    /// minimal but realistic daemon state should use this to keep
    /// session-handler / dispatch tests independent from the boot path.
    /// The carried Config is `Config::default()` — sufficient for
    /// failure-path tests but won't produce a working Agent build.
    #[doc(hidden)]
    pub fn for_tests_minimal() -> Self {
        Self::new(Arc::new(Config::default()))
    }

    /// Initialize with an opened CortexStore. Called by the daemon startup
    /// path after loading config.
    pub fn with_cortex(mut self, store: Arc<CortexStore>) -> Self {
        self.cortex = Some(store);
        self
    }

    /// Borrow the cortex store, if enabled.
    pub fn cortex(&self) -> Option<&Arc<CortexStore>> {
        self.cortex.as_ref()
    }

    /// Borrow the daemon's loaded `Config`. Used by lazy-build paths
    /// (e.g. `SessionState::ensure_agent`) so handlers don't have to
    /// thread a separate Config reference through every call.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Count of successful config reloads applied since startup.
    pub fn reloads_applied(&self) -> u64 {
        self.reloads_applied.load(Ordering::SeqCst)
    }

    /// Slice 0 stub for config reload. Reads the file at `path`,
    /// validates it parses as TOML, and bumps the reload counter on
    /// success. Slice 2 will replace this with the actual Provider
    /// rebuild + Agent swap.
    pub async fn apply_config_stub(&self, path: &Path) {
        match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|s| toml::from_str::<toml::Value>(&s).map_err(|e| e.to_string()))
        {
            Ok(_) => {
                self.reloads_applied.fetch_add(1, Ordering::SeqCst);
                tracing::info!(?path, "config_watch: reload applied (stub)");
            }
            Err(e) => {
                tracing::warn!(error = %e, ?path, "config_watch: reload failed; keeping current config");
            }
        }
    }

    /// Borrow the session map. Used by handlers that need to look
    /// up or mutate per-session state.
    pub fn sessions(&self) -> &SessionMap {
        &self.sessions
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Signal shutdown. Idempotent and latching — once called, every
    /// existing AND future call to [`Self::shutdown_signal`] observes
    /// the latched `true` value via `borrow()`, and any receiver
    /// acquired *before* the signal will resolve `changed().await`
    /// immediately. No registration ordering required (unlike `Notify`,
    /// which only wakes already-parked waiters).
    pub fn signal_shutdown(&self) {
        // Ignore send error: receivers only get dropped when the daemon
        // is already torn down past the point of caring.
        let _ = self.shutdown_tx.send(true);
    }

    /// Acquire a watch receiver. Await `recv.changed().await` (or check
    /// `*recv.borrow()`) to observe shutdown. Safe to call before or
    /// after [`Self::signal_shutdown`] — late callers see the latched
    /// `true` value via `borrow()`.
    pub fn shutdown_signal(&self) -> watch::Receiver<bool> {
        self.shutdown_rx.clone()
    }

    /// Queue a config reload (eventually drained by config_watch's
    /// idle-deferred loop in Task 0.10). Idempotent.
    pub fn queue_reload(&self) {
        self.reload.notify_waiters();
    }

    pub fn reload_signal(&self) -> Arc<Notify> {
        self.reload.clone()
    }

    /// Returns one descriptor per live session — id, in_flight,
    /// last_activity_secs_ago. Replaces the Slice 0 Task 0.6 stub.
    pub fn session_descriptors(&self) -> Vec<serde_json::Value> {
        self.sessions.descriptors()
    }
}

impl Default for DaemonState {
    fn default() -> Self {
        Self::new(Arc::new(Config::default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_is_none_by_default_and_some_after_with_pool() {
        // Slice 3: production startup installs the pool via with_pool;
        // pre-install code paths see None and fall back to per-session
        // adapter construction.
        let state = DaemonState::for_tests_minimal();
        assert!(state.pool().is_none());

        let pool = Arc::new(RuntimeResourcePool::for_tests());
        let state = state.with_pool(Arc::clone(&pool));
        let installed = state.pool().expect("pool installed");
        assert!(Arc::ptr_eq(installed, &pool));
    }
}
