//! Long-lived daemon process state. Holds the shutdown / reload signals
//! and (once Slice 1+ adds them) the SessionMap and SharedResources.

use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Notify, watch};

/// Per-process daemon state, shared across all connections.
pub struct DaemonState {
    started_at: Instant,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    reload: Arc<Notify>,
}

impl DaemonState {
    pub fn new() -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            started_at: Instant::now(),
            shutdown_tx,
            shutdown_rx,
            reload: Arc::new(Notify::new()),
        }
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
