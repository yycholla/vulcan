//! Daemon process lifecycle — PID file acquire / release with stale detection.
//!
//! Uses `O_CREAT | O_EXCL` to atomically prevent two daemons from running
//! simultaneously. `acquire_or_replace_stale` handles the case where a
//! previous daemon crashed without releasing — checks if the recorded PID
//! is still alive (via `kill(pid, None)`) and overwrites if not.

use std::fs::{File, OpenOptions, Permissions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tokio::net::UnixListener;

/// Owner of a `daemon.pid` file. The file exists for the lifetime of this
/// struct; `Drop` removes it.
pub struct PidFile {
    path: PathBuf,
    _file: File,
}

impl PidFile {
    /// Strict acquire. Fails (`io::ErrorKind::AlreadyExists`) if the file
    /// already exists, regardless of whether the recorded PID is alive.
    /// Use `acquire_or_replace_stale` for the daemon-startup path.
    pub fn acquire(path: &Path) -> std::io::Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true) // O_CREAT | O_EXCL
            .mode(0o600)
            .open(path)?;
        writeln!(file, "{}", std::process::id())?;
        Ok(Self {
            path: path.to_path_buf(),
            _file: file,
        })
    }

    /// Acquire, but if a stale PID file exists (recorded process is dead),
    /// remove it and acquire fresh. Returns an error if the recorded PID
    /// is still alive (another daemon is running) or if the file contents
    /// are unparseable.
    pub fn acquire_or_replace_stale(path: &Path) -> std::io::Result<Self> {
        match Self::acquire(path) {
            Ok(f) => Ok(f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let mut s = String::new();
                File::open(path)?.read_to_string(&mut s)?;
                let pid: i32 = s
                    .trim()
                    .parse()
                    .map_err(|_| std::io::Error::other("malformed pid file"))?;
                if is_alive(pid) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!("daemon already running (pid {pid})"),
                    ));
                }
                std::fs::remove_file(path)?;
                Self::acquire(path)
            }
            Err(e) => Err(e),
        }
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Probe whether `pid` corresponds to a live process via `kill(pid, None)`.
/// On Unix this performs no actual signal delivery; it just checks that
/// the process exists and is signal-deliverable by the current user.
fn is_alive(pid: i32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid), None).is_ok()
}

/// Owns the daemon's listening socket file. Ensures 0600 perms,
/// cleans up stale leftovers, and unlinks on drop.
///
/// Distinguishes "stale leftover" (some non-socket file at the path,
/// or a dead socket no one is listening on) from "live daemon"
/// (something accepting connections). Stale files are removed; live
/// sockets cause `AddrInUse` rather than silent takeover.
pub struct SocketBinder {
    pub listener: UnixListener,
    path: PathBuf,
}

impl SocketBinder {
    pub async fn bind(path: &Path) -> std::io::Result<Self> {
        // If something exists at the path, decide stale vs live.
        if path.exists() {
            match tokio::net::UnixStream::connect(path).await {
                Ok(_) => {
                    // Live daemon (or some other server) is accepting on it.
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        format!("socket already in use at {}", path.display()),
                    ));
                }
                Err(_) => {
                    // Stale: not a live socket, or a non-socket file. Remove.
                    std::fs::remove_file(path)?;
                }
            }
        }

        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, Permissions::from_mode(0o600))?;
        Ok(Self {
            listener,
            path: path.to_path_buf(),
        })
    }
}

impl Drop for SocketBinder {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
