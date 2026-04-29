#![cfg(feature = "daemon")]
//! End-to-end coverage for `vulcan daemon ...` (YYC-266 Slice 0 Task 0.8).
//!
//! Exercises the full start (detached) → status → reload → stop loop
//! against a real `vulcan` binary, with `VULCAN_HOME` redirected to a
//! tempdir so the test can't collide with a developer's running daemon.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use std::time::Duration;
use tempfile::tempdir;

fn vulcan_with_home(home: &Path) -> Command {
    let mut c = Command::cargo_bin("vulcan").unwrap();
    c.env("VULCAN_HOME", home);
    c.env("RUST_LOG", "warn");
    c
}

fn wait_for_socket(home: &Path, timeout: Duration) -> bool {
    let sock = home.join("vulcan.sock");
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn daemon_start_detach_status_stop() {
    let dir = tempdir().unwrap();

    // Detached start returns once the socket is reachable.
    vulcan_with_home(dir.path())
        .args(["daemon", "start", "--detach"])
        .assert()
        .success();

    assert!(
        wait_for_socket(dir.path(), Duration::from_secs(5)),
        "socket must come up within 5s"
    );

    // Status returns JSON containing pid + uptime_secs.
    vulcan_with_home(dir.path())
        .args(["daemon", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pid"))
        .stdout(predicate::str::contains("uptime_secs"));

    // Reload returns OK (handler queues the reload signal).
    vulcan_with_home(dir.path())
        .args(["daemon", "reload"])
        .assert()
        .success();

    // Stop sends shutdown.
    vulcan_with_home(dir.path())
        .args(["daemon", "stop"])
        .assert()
        .success();

    // Wait briefly for the socket file to disappear after shutdown.
    let sock = dir.path().join("vulcan.sock");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while sock.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn daemon_status_fails_when_no_daemon() {
    let dir = tempdir().unwrap();
    vulcan_with_home(dir.path())
        .args(["daemon", "status"])
        .assert()
        .failure(); // no socket present
}

/// Locked-design check: the daemon's socket file must be 0600. Same-user
/// trust model (ssh-agent precedent) — file mode is the only access
/// control on the IPC surface.
#[test]
fn daemon_socket_is_0600() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    vulcan_with_home(dir.path())
        .args(["daemon", "start", "--detach"])
        .assert()
        .success();
    assert!(
        wait_for_socket(dir.path(), Duration::from_secs(5)),
        "socket must come up within 5s"
    );

    let sock = dir.path().join("vulcan.sock");
    let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "socket must be owner-only RW");

    vulcan_with_home(dir.path())
        .args(["daemon", "stop"])
        .assert()
        .success();
}

/// PID file must also be 0600 — it sits next to the socket and is
/// written under the same threat model.
#[test]
fn daemon_pid_file_is_0600() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    vulcan_with_home(dir.path())
        .args(["daemon", "start", "--detach"])
        .assert()
        .success();
    assert!(
        wait_for_socket(dir.path(), Duration::from_secs(5)),
        "socket must come up within 5s"
    );

    let pid = dir.path().join("daemon.pid");
    assert!(pid.exists(), "pid file present once socket is up");
    let mode = std::fs::metadata(&pid).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "pid file must be owner-only RW");

    vulcan_with_home(dir.path())
        .args(["daemon", "stop"])
        .assert()
        .success();
}
