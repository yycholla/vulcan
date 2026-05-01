//! `vulcan daemon ...` subcommand action handlers (YYC-266 Slice 0
//! Task 0.8).
//!
//! Translates the parsed [`DaemonAction`] into either an in-process
//! server bring-up (`start`) or a Unix-socket round trip to a running
//! daemon (`stop`/`status`/`reload`). `install` is stubbed pending Task
//! 0.12.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;

use crate::cli::DaemonAction;
use crate::config::vulcan_home;
use crate::daemon::lifecycle::PidFile;
use crate::daemon::protocol::{Request, Response, read_frame_bytes, write_request};
use crate::daemon::server::Server;
use crate::daemon::state::DaemonState;

/// Entry point dispatched from `main.rs` for `vulcan daemon ...`.
pub async fn run(action: DaemonAction) -> anyhow::Result<()> {
    match action {
        DaemonAction::Start { detach } => start(detach).await,
        DaemonAction::Stop { force } => stop(force).await,
        DaemonAction::Status => status().await,
        DaemonAction::Reload => reload().await,
        DaemonAction::Install { systemd } => install(systemd).await,
    }
}

fn home_paths() -> (PathBuf, PathBuf) {
    let home = vulcan_home();
    (home.join("daemon.pid"), home.join("vulcan.sock"))
}

async fn start(detach: bool) -> anyhow::Result<()> {
    let home = vulcan_home();
    std::fs::create_dir_all(&home).with_context(|| format!("creating {}", home.display()))?;
    let (pid_path, sock_path) = home_paths();

    if detach {
        return spawn_detached(&sock_path).await;
    }

    let _pidfile = PidFile::acquire_or_replace_stale(&pid_path)
        .with_context(|| format!("acquiring pid file {}", pid_path.display()))?;

    // YYC-266 Slice 1: boot the CortexStore once so all CLI/TUI clients
    // share it without fighting over the redb exclusive lock.
    let config = crate::config::Config::load()?;
    let mut state = DaemonState::new(Arc::new(config.clone()));
    if config.cortex.enabled {
        match crate::memory::cortex::CortexStore::try_open(&config.cortex) {
            Ok(store) => {
                tracing::info!("daemon: cortex store loaded");
                state = state.with_cortex(store);
            }
            Err(e) => {
                tracing::warn!("daemon: cortex store failed to open: {e:#}");
                state = state.with_cortex_error(format!("{e:#}"));
            }
        }
    }

    // Slice 3: open the daemon's RuntimeResourcePool so subsequent
    // session/agent assembly reuses one SessionStore connection, one
    // run/artifact store, and one orchestration store across the
    // whole process. Pool open failure is fatal — without it the
    // session paths can't run.
    let mut pool_builder = crate::runtime_pool::RuntimeResourcePool::try_new()
        .context("opening daemon RuntimeResourcePool")?;
    if let Some(cortex) = state.cortex() {
        pool_builder = pool_builder.with_cortex_store(Arc::clone(cortex));
    }
    let pool = Arc::new(pool_builder);
    let disabled = config
        .extensions
        .apply_to_registry(&pool.extension_registry());
    if disabled > 0 {
        tracing::info!(disabled, "daemon: extension config disabled extensions");
    }
    state = state.with_pool(Arc::clone(&pool));

    // YYC-266 Slice 2/3: boot the warm Agent and install it into the
    // "main" session. Additional sessions are created on-demand.
    match build_daemon_agent(&config, Arc::clone(&pool)).await {
        Ok(agent) => {
            tracing::info!("daemon: agent loaded (model={})", agent.active_model());
            if let Some(main) = state.sessions().get("main") {
                main.set_agent(agent);
            }
        }
        Err(e) => {
            tracing::warn!("daemon: agent failed to build: {e}");
        }
    }

    let state = Arc::new(state);
    let server = Server::bind(&sock_path, state.clone())
        .await
        .with_context(|| format!("binding socket {}", sock_path.display()))?;

    install_signal_handlers(state.clone());

    // YYC-266 Slice 3 Task 3.2: idle-eviction sweeper for non-"main"
    // sessions. The handle is `_`-bound on purpose — the loop
    // self-terminates on the watch-based shutdown signal.
    let idle_ttl = Duration::from_secs(config.daemon.session_idle_ttl_secs);
    let sweep_interval = Duration::from_secs(config.daemon.eviction_sweep_interval_secs);
    let _evictor_handle = crate::daemon::eviction::spawn(state.clone(), idle_ttl, sweep_interval);

    server.run().await;
    Ok(())
}

async fn build_daemon_agent(
    config: &crate::config::Config,
    pool: Arc<crate::runtime_pool::RuntimeResourcePool>,
) -> anyhow::Result<crate::agent::Agent> {
    crate::agent::Agent::builder(config)
        .with_pool(pool)
        .build()
        .await
}

fn install_signal_handlers(state: Arc<DaemonState>) {
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to install SIGTERM handler");
                return;
            }
        };
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to install SIGINT handler");
                return;
            }
        };
        tokio::select! {
            _ = sigterm.recv() => tracing::info!("daemon: SIGTERM received"),
            _ = sigint.recv() => tracing::info!("daemon: SIGINT received"),
        }
        state.signal_shutdown();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn daemon_agent_build_uses_runtime_pool_session_store() {
        let mut config = crate::config::Config::default();
        config.provider.base_url = "http://127.0.0.1:11434/v1".into();
        config.provider.disable_catalog = true;
        let pool = Arc::new(crate::runtime_pool::RuntimeResourcePool::for_tests());

        let agent = build_daemon_agent(&config, Arc::clone(&pool))
            .await
            .unwrap();
        let session_id = agent.session_id().to_string();
        agent
            .memory()
            .save_messages(
                &session_id,
                &[crate::provider::Message::User {
                    content: "from daemon agent".into(),
                }],
            )
            .unwrap();

        let loaded = pool.session_store().load_history(&session_id).unwrap();
        assert!(
            matches!(
                loaded.as_deref().and_then(|messages| messages.first()),
                Some(crate::provider::Message::User { content }) if content == "from daemon agent"
            ),
            "daemon-built main Agent must share the RuntimeResourcePool SessionStore"
        );
    }
}

async fn spawn_detached(sock_path: &Path) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("locating own exe")?;
    // The detached child re-execs `vulcan daemon start` (without
    // `--detach`) so it runs the in-process server path. stdio is
    // pointed at `/dev/null` so the parent can return cleanly.
    std::process::Command::new(&exe)
        .args(["daemon", "start"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("spawning detached daemon")?;

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if tokio::net::UnixStream::connect(sock_path).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("daemon did not come up within 5s")
}

async fn stop(force: bool) -> anyhow::Result<()> {
    call("daemon.shutdown", serde_json::json!({ "force": force })).await?;
    Ok(())
}

async fn status() -> anyhow::Result<()> {
    let result = call("daemon.status", serde_json::json!({})).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

async fn reload() -> anyhow::Result<()> {
    call("daemon.reload", serde_json::json!({})).await?;
    Ok(())
}

async fn install(systemd: bool) -> anyhow::Result<()> {
    if !systemd {
        anyhow::bail!("`daemon install` requires `--systemd` (only target supported in Slice 0)");
    }
    let unit_path = crate::daemon::install::install_systemd_default()?;
    println!("wrote {}", unit_path.display());
    println!("enable with: systemctl --user enable --now vulcan.service");
    Ok(())
}

async fn call(method: &str, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let (_, sock_path) = home_paths();
    let mut stream = tokio::net::UnixStream::connect(&sock_path)
        .await
        .with_context(|| format!("connecting to {}", sock_path.display()))?;
    let req = Request {
        version: 1,
        id: format!("cli-{method}"),
        session: "main".into(),
        method: method.into(),
        params,
        frontend_capabilities: crate::extensions::FrontendCapability::text_only(),
    };
    write_request(&mut stream, &req)
        .await
        .context("writing request frame")?;
    let body = read_frame_bytes(&mut stream)
        .await
        .context("reading response frame")?;
    let resp: Response = serde_json::from_slice(&body).context("decoding response")?;
    if let Some(err) = resp.error {
        anyhow::bail!("{}: {}", err.code, err.message);
    }
    Ok(resp.result.unwrap_or_default())
}
