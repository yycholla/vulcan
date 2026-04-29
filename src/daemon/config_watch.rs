//! Filesystem watcher that drives idle-deferred config reload.
//!
//! Per YYC-266 design: edits to the daemon's config files trigger a
//! reload only when no session has `in_flight = true`. While a session
//! is mid-turn, the reload intent is queued and drained as soon as the
//! daemon goes idle. Burst edits are coalesced via a tokio mpsc drain.
//!
//! Slice 0 plumbs the mechanism: detect changes, defer until idle,
//! call [`crate::daemon::state::DaemonState::apply_config_stub`].
//! The actual Provider rebuild + Agent swap lands in Slice 2.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

use crate::daemon::state::DaemonState;

/// Owner of the active filesystem watcher. Drop the value to stop
/// watching; the apply task exits when its sender side is dropped.
pub struct ConfigWatcher {
    /// Held to keep the watcher alive; not read directly.
    _watcher: notify::RecommendedWatcher,
}

impl ConfigWatcher {
    /// Start watching `config_path` for modifications. On change, queue
    /// a reload that fires once the daemon is idle (no session has
    /// `in_flight = true`).
    pub fn start(config_path: &Path, state: Arc<DaemonState>) -> notify::Result<Self> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let watch_tx = tx.clone();

        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                match res {
                    Ok(ev) => {
                        if matches!(
                            ev.kind,
                            notify::EventKind::Modify(_) | notify::EventKind::Create(_)
                        ) {
                            // Send may fail if receiver dropped — ignore, watcher about to die anyway.
                            let _ = watch_tx.send(());
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "config_watch: notify error");
                    }
                }
            })?;

        // notify accepts a path; we watch the config file itself, not the dir.
        watcher.watch(config_path, RecursiveMode::NonRecursive)?;

        let cfg_path = config_path.to_path_buf();
        tokio::spawn(apply_loop(rx, state, cfg_path));

        Ok(Self { _watcher: watcher })
    }
}

async fn apply_loop(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<()>,
    state: Arc<DaemonState>,
    cfg_path: PathBuf,
) {
    while rx.recv().await.is_some() {
        // Settle period: wait briefly so a burst of edits (notify often
        // emits 2-3 events per save; editors may also emit several
        // saves in quick succession) coalesces into one apply.
        tokio::time::sleep(Duration::from_millis(75)).await;
        // Drain any burst-queued events so we don't reload N times for
        // one save.
        while rx.try_recv().is_ok() {}

        // Defer until daemon is idle (no session in_flight). Poll at
        // 100ms; the loop exits when sessions go idle OR when the
        // process is shutting down.
        let mut shutdown = state.shutdown_signal();
        loop {
            if !state.sessions().any_in_flight() {
                break;
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        tracing::debug!("config_watch: shutdown observed; abandoning queued reload");
                        return;
                    }
                }
            }
        }

        state.apply_config_stub(&cfg_path).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::DaemonState;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::tempdir;

    /// Helper: write file, sleep briefly so notify's debounce/coalescing
    /// has time to fire.
    async fn write_and_settle(path: &std::path::Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    #[tokio::test]
    async fn reload_applies_when_idle() {
        let dir = tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "key = 1").unwrap();

        let state = Arc::new(DaemonState::new());
        let _watcher = ConfigWatcher::start(&cfg, state.clone()).unwrap();

        let baseline = state.reloads_applied();
        write_and_settle(&cfg, "key = 2").await;

        // give the apply task time
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            state.reloads_applied(),
            baseline + 1,
            "edit during idle should apply once"
        );
    }

    #[tokio::test]
    async fn reload_deferred_while_session_in_flight() {
        let dir = tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "key = 1").unwrap();

        let state = Arc::new(DaemonState::new());
        let main = state.sessions().get("main").unwrap();
        *main.in_flight.lock() = true;

        let _watcher = ConfigWatcher::start(&cfg, state.clone()).unwrap();
        let baseline = state.reloads_applied();

        write_and_settle(&cfg, "key = 2").await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            state.reloads_applied(),
            baseline,
            "in-flight session blocks apply"
        );

        // Clear in-flight; reload should fire shortly.
        *main.in_flight.lock() = false;
        // Watcher polls in_flight every ~100ms in the deferred branch.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            state.reloads_applied(),
            baseline + 1,
            "becoming idle drains queued reload"
        );
    }

    #[tokio::test]
    async fn rapid_edits_coalesce() {
        let dir = tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "key = 1").unwrap();

        let state = Arc::new(DaemonState::new());
        let _watcher = ConfigWatcher::start(&cfg, state.clone()).unwrap();
        let baseline = state.reloads_applied();

        // Five edits in quick succession
        for i in 2..7 {
            std::fs::write(&cfg, format!("key = {i}")).unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Wait for settling + apply
        tokio::time::sleep(Duration::from_millis(500)).await;

        let applied = state.reloads_applied();
        assert!(applied >= baseline + 1, "at least one reload");
        assert!(
            applied <= baseline + 3,
            "five rapid edits coalesce into ≤3 applies, got {}",
            applied - baseline
        );
    }
}
