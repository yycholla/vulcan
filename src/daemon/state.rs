//! Long-lived daemon process state. Holds the shutdown / reload signals
//! and (once Slice 1+ adds them) the SessionMap and SharedResources.

use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Notify;

/// Per-process daemon state, shared across all connections.
pub struct DaemonState {
    started_at: Instant,
    shutdown: Arc<Notify>,
    reload: Arc<Notify>,
}

impl DaemonState {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            shutdown: Arc::new(Notify::new()),
            reload: Arc::new(Notify::new()),
        }
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Notify waiters that the daemon should shut down. Idempotent.
    pub fn signal_shutdown(&self) {
        self.shutdown.notify_waiters();
    }

    /// Acquire a clone of the shutdown notifier; await `.notified()` to
    /// observe shutdown.
    pub fn shutdown_signal(&self) -> Arc<Notify> {
        self.shutdown.clone()
    }

    /// Queue a config reload (eventually drained by config_watch's
    /// idle-deferred loop in Task 0.10). Idempotent.
    pub fn queue_reload(&self) {
        self.reload.notify_waiters();
    }

    pub fn reload_signal(&self) -> Arc<Notify> {
        self.reload.clone()
    }

    /// Slice 0 stub: returns empty array. Slice 0 Task 0.9 (SessionMap)
    /// will replace this with actual descriptors.
    pub fn session_descriptors(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }
}

impl Default for DaemonState {
    fn default() -> Self {
        Self::new()
    }
}
