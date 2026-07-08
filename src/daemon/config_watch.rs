//! Filesystem watcher that drives idle-deferred config reload.
//!
//! Per YYC-266 design: edits to the daemon's config files trigger a
//! reload only when no session has `in_flight = true`. While a session
//! is mid-turn, the reload intent is queued and drained as soon as the
//! daemon goes idle. Burst edits are coalesced via a tokio mpsc drain.

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
    /// Start watching the daemon config directory for modifications. On
    /// change, queue a reload that fires once the daemon is idle (no
    /// session has `in_flight = true`).
    pub fn start(config_dir: &Path, state: Arc<DaemonState>) -> notify::Result<Self> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let watch_tx = tx.clone();
        let watched_dir = config_dir.to_path_buf();

        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(ev) => {
                    if matches!(
                        ev.kind,
                        notify::EventKind::Modify(_) | notify::EventKind::Create(_)
                    ) && ev.paths.iter().any(|path| is_config_fragment(path))
                    {
                        let _ = watch_tx.send(());
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "config_watch: notify error");
                }
            })?;

        watcher.watch(config_dir, RecursiveMode::NonRecursive)?;
        tokio::spawn(apply_loop(rx, state, watched_dir));

        Ok(Self { _watcher: watcher })
    }
}

fn is_config_fragment(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("config.toml" | "keybinds.toml" | "providers.toml")
    )
}

async fn apply_loop(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<()>,
    state: Arc<DaemonState>,
    config_dir: PathBuf,
) {
    while rx.recv().await.is_some() {
        tokio::time::sleep(Duration::from_millis(75)).await;
        while rx.try_recv().is_ok() {}

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

        let report = state.reload_from_dir(&config_dir).await;
        tracing::info!(
            status = %report.status,
            sessions_rebuilt = report.sessions_rebuilt,
            restart_required = ?report.restart_required,
            "config_watch: reload attempted"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::DaemonState;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::tempdir;

    async fn write_and_settle(path: &std::path::Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    #[tokio::test]
    async fn reload_applies_when_idle() {
        let dir = tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(
            &cfg,
            "[provider]\nbase_url = \"http://127.0.0.1:11434/v1\"\nmodel = \"qwen2.5:7b\"\ndisable_catalog = true\n",
        )
        .unwrap();

        let mut baseline = crate::config::Config::default();
        baseline.provider.base_url = "http://127.0.0.1:11434/v1".into();
        baseline.provider.model = "qwen2.5:7b".into();
        baseline.provider.disable_catalog = true;
        let pool = Arc::new(crate::runtime_pool::RuntimeResourcePool::for_tests().await);
        let state = Arc::new(
            DaemonState::for_tests_with_home(Arc::new(baseline), dir.path()).with_pool(pool),
        );
        let _watcher = ConfigWatcher::start(dir.path(), state.clone()).unwrap();

        let baseline = state.reloads_applied();
        write_and_settle(
            &cfg,
            "[provider]\nbase_url = \"http://127.0.0.1:11434/v1\"\nmodel = \"qwen2.5:7b\"\ndisable_catalog = true\n[tools]\nweb = true\n",
        )
        .await;

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(state.reloads_applied(), baseline + 1);
        assert_eq!(state.last_reload_report().unwrap().status, "applied");
    }

    #[tokio::test]
    async fn reload_deferred_while_session_in_flight() {
        let dir = tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(
            &cfg,
            "[provider]\nbase_url = \"http://127.0.0.1:11434/v1\"\nmodel = \"qwen2.5:7b\"\ndisable_catalog = true\n",
        )
        .unwrap();

        let mut baseline_cfg = crate::config::Config::default();
        baseline_cfg.provider.base_url = "http://127.0.0.1:11434/v1".into();
        baseline_cfg.provider.model = "qwen2.5:7b".into();
        baseline_cfg.provider.disable_catalog = true;
        let pool = Arc::new(crate::runtime_pool::RuntimeResourcePool::for_tests().await);
        let state = Arc::new(
            DaemonState::for_tests_with_home(Arc::new(baseline_cfg), dir.path()).with_pool(pool),
        );
        let main = state.sessions().get("main").unwrap();
        *main.in_flight.lock() = true;

        let _watcher = ConfigWatcher::start(dir.path(), state.clone()).unwrap();
        let baseline = state.reloads_applied();

        write_and_settle(
            &cfg,
            "[provider]\nbase_url = \"http://127.0.0.1:11434/v1\"\nmodel = \"qwen2.5:14b\"\ndisable_catalog = true\n",
        )
        .await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(state.reloads_applied(), baseline);

        *main.in_flight.lock() = false;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(state.reloads_applied(), baseline + 1);
    }

    #[tokio::test]
    async fn reload_reports_legacy_fragment_advice() {
        let dir = tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(
            &cfg,
            "[provider]\nbase_url = \"http://127.0.0.1:11434/v1\"\nmodel = \"qwen2.5:7b\"\ndisable_catalog = true\n\n[keybinds]\ncancel = \"ctrl+c\"\n",
        )
        .unwrap();

        let mut baseline = crate::config::Config::default();
        baseline.provider.base_url = "http://127.0.0.1:11434/v1".into();
        baseline.provider.model = "qwen2.5:7b".into();
        baseline.provider.disable_catalog = true;
        let pool = Arc::new(crate::runtime_pool::RuntimeResourcePool::for_tests().await);
        let state = Arc::new(
            DaemonState::for_tests_with_home(Arc::new(baseline), dir.path()).with_pool(pool),
        );

        let report = state.reload_from_dir(dir.path()).await;
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diag| diag.code == "LEGACY_KEYBINDS_INLINE")
        );
    }
}
