//! Auto-start: detect missing/dead daemon, spawn one, wait for socket.
//!
//! Distinct from `vulcan daemon start --detach` — the client polls the
//! socket itself instead of trusting the spawned process to do it. This
//! keeps the contract simple: the spawned `vulcan daemon start` is a
//! plain blocking process, and the client treats its presence as a
//! lifecycle dependency.

use std::path::Path;
use std::time::{Duration, Instant};

use super::errors::{ClientError, ClientResult};

const AUTOSTART_TIMEOUT_SECS: u64 = 5;
const POLL_INTERVAL_MS: u64 = 50;

/// Ensure a daemon is reachable at `sock_path`. If not, fork+exec
/// `vulcan daemon start` and poll for the socket to come up.
///
/// Stale-socket cleanup: if the path exists but `connect(2)` fails, we
/// remove it before spawning. The new daemon's `bind(2)` would otherwise
/// fail with `EADDRINUSE` against the old inode.
pub async fn ensure_daemon(sock_path: &Path) -> ClientResult<()> {
    if can_connect(sock_path).await {
        return Ok(());
    }

    // Stale socket file? Clean up; bind in the fresh process will fail
    // otherwise. Best-effort — ignore errors (race with another client
    // that just succeeded in starting a daemon is fine; we'll re-poll).
    if sock_path.exists() {
        let _ = std::fs::remove_file(sock_path);
    }

    let exe = std::env::current_exe()?;
    std::process::Command::new(&exe)
        .args(["daemon", "start"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(ClientError::Io)?;

    let deadline = Instant::now() + Duration::from_secs(AUTOSTART_TIMEOUT_SECS);
    while Instant::now() < deadline {
        if can_connect(sock_path).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
    Err(ClientError::AutostartFailed {
        timeout_secs: AUTOSTART_TIMEOUT_SECS,
    })
}

async fn can_connect(path: &Path) -> bool {
    tokio::net::UnixStream::connect(path).await.is_ok()
}
