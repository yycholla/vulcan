#![cfg(feature = "daemon")]
//! End-to-end coverage for the in-tree `vulcan-client` (YYC-266 Slice 0
//! Task 0.11).
//!
//! Drives a real `vulcan` binary at the hidden `__ping` subcommand under
//! a temp `VULCAN_HOME`, verifying that:
//!
//! * a cold invocation auto-starts a daemon and round-trips `daemon.ping`;
//! * a warm invocation reuses the running daemon (single PID across calls);
//! * a stale (non-socket) file at the socket path is cleaned up before
//!   spawning;
//! * concurrent invocations settle on a single live daemon (PID file is
//!   acquired exclusively).

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::tempdir;

fn vulcan_with_home(home: &Path) -> Command {
    let mut c = match std::env::var_os("CARGO_BIN_EXE_vulcan") {
        Some(path) => Command::new(path),
        None => Command::new(vulcan_bin_path()),
    };
    c.env("VULCAN_HOME", home);
    c.env("RUST_LOG", "warn");
    c
}

fn vulcan_bin_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("debug")
        .join("vulcan")
}

fn wait_for_socket_gone(home: &Path, timeout: Duration) {
    let sock = home.join("vulcan.sock");
    let deadline = std::time::Instant::now() + timeout;
    while sock.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn cold_invocation_autostarts_daemon() {
    let dir = tempdir().unwrap();
    vulcan_with_home(dir.path())
        .args(["__ping"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pong"));

    // Daemon should be running now (socket present)
    assert!(dir.path().join("vulcan.sock").exists());

    // Cleanup: stop daemon
    vulcan_with_home(dir.path())
        .args(["daemon", "stop"])
        .assert()
        .success();
    wait_for_socket_gone(dir.path(), Duration::from_secs(2));
}

#[test]
fn second_invocation_reuses_daemon() {
    let dir = tempdir().unwrap();
    vulcan_with_home(dir.path())
        .args(["__ping"])
        .assert()
        .success();

    let pid1 =
        std::fs::read_to_string(dir.path().join("daemon.pid")).expect("first invocation wrote pid");

    vulcan_with_home(dir.path())
        .args(["__ping"])
        .assert()
        .success();

    let pid2 = std::fs::read_to_string(dir.path().join("daemon.pid"))
        .expect("second invocation pid still present");

    assert_eq!(
        pid1, pid2,
        "second invocation must reuse daemon (single PID)"
    );

    vulcan_with_home(dir.path())
        .args(["daemon", "stop"])
        .assert()
        .success();
    wait_for_socket_gone(dir.path(), Duration::from_secs(2));
}

#[test]
fn autostart_handles_stale_socket() {
    let dir = tempdir().unwrap();
    // Plant a stale (non-socket) file at the socket path
    std::fs::write(dir.path().join("vulcan.sock"), "stale").unwrap();

    vulcan_with_home(dir.path())
        .args(["__ping"])
        .assert()
        .success();

    vulcan_with_home(dir.path())
        .args(["daemon", "stop"])
        .assert()
        .success();
    wait_for_socket_gone(dir.path(), Duration::from_secs(2));
}

#[test]
fn autostart_race_settles_to_one_daemon() {
    let dir = tempdir().unwrap();
    let mut handles = vec![];
    for _ in 0..4 {
        let p = dir.path().to_path_buf();
        handles.push(std::thread::spawn(move || {
            vulcan_with_home(&p).args(["__ping"]).assert().success();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    let pid =
        std::fs::read_to_string(dir.path().join("daemon.pid")).expect("a single pid file remains");
    let _: i32 = pid.trim().parse().expect("valid pid");

    vulcan_with_home(dir.path())
        .args(["daemon", "stop"])
        .assert()
        .success();
    wait_for_socket_gone(dir.path(), Duration::from_secs(2));
}

#[test]
fn ping_subcommand_hidden_from_help() {
    // `__ping` is internal/test-only; users shouldn't see it in --help.
    let dir = tempdir().unwrap();
    vulcan_with_home(dir.path())
        .args(["--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("__ping").not());
}
