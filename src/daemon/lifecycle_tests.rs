use super::lifecycle::*;
use tempfile::tempdir;

#[test]
fn pid_file_create_excl_rejects_second_writer() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("daemon.pid");
    let _first = PidFile::acquire(&path).expect("first acquire OK");
    let second = PidFile::acquire(&path);
    assert!(second.is_err(), "second acquire on live PidFile must fail");
}

#[test]
fn pid_file_released_on_drop() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("daemon.pid");
    {
        let _f = PidFile::acquire(&path).unwrap();
        assert!(path.exists(), "file exists while held");
    } // dropped here
    assert!(!path.exists(), "drop removes the pid file");
    let again = PidFile::acquire(&path);
    assert!(again.is_ok(), "re-acquire after drop OK");
}

#[test]
fn pid_file_writes_current_pid() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("daemon.pid");
    let _f = PidFile::acquire(&path).unwrap();
    let contents = std::fs::read_to_string(&path).unwrap();
    let pid: i32 = contents
        .trim()
        .parse()
        .expect("file contains an integer pid");
    assert_eq!(pid, std::process::id() as i32);
}

#[test]
fn pid_file_acquire_or_replace_stale_overwrites_dead_pid() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("daemon.pid");
    // Write a fake PID that's almost certainly dead (i32::MAX).
    std::fs::write(&path, format!("{}\n", i32::MAX)).unwrap();
    let _f = PidFile::acquire_or_replace_stale(&path).expect("stale PID overwritten");
    let contents = std::fs::read_to_string(&path).unwrap();
    let pid: i32 = contents.trim().parse().unwrap();
    assert_eq!(pid, std::process::id() as i32, "PID file now holds our pid");
}

#[test]
fn pid_file_acquire_or_replace_stale_rejects_live_pid() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("daemon.pid");
    // Use our own PID — definitely alive.
    let live_pid = std::process::id();
    std::fs::write(&path, format!("{live_pid}\n")).unwrap();
    let result = PidFile::acquire_or_replace_stale(&path);
    assert!(result.is_err(), "live PID must not be overwritten");
    let err = result.err().unwrap();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(
        err.to_string().contains("already running"),
        "error message must say 'already running', got: {err}"
    );
}

#[test]
fn pid_file_acquire_or_replace_stale_rejects_malformed_pid() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("daemon.pid");
    std::fs::write(&path, "garbage").unwrap();
    let result = PidFile::acquire_or_replace_stale(&path);
    assert!(
        result.is_err(),
        "malformed pid file must error, not silently overwrite"
    );
}

#[test]
fn pid_file_perms_are_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let path = dir.path().join("daemon.pid");
    let _f = PidFile::acquire(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "pid file must be 0600 (owner-only)");
}

use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn socket_binder_creates_0600_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vulcan.sock");
    let _bind = SocketBinder::bind(&path).await.unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "socket file must be 0600");
}

#[tokio::test]
async fn socket_binder_unlinks_on_drop() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vulcan.sock");
    {
        let _b = SocketBinder::bind(&path).await.unwrap();
        assert!(path.exists(), "socket file exists while bound");
    }
    assert!(!path.exists(), "drop unlinks socket file");
}

#[tokio::test]
async fn socket_binder_replaces_stale_file() {
    // A stale leftover file (not a live socket) must be cleaned up,
    // because UnixListener::bind would otherwise fail with EADDRINUSE.
    let dir = tempdir().unwrap();
    let path = dir.path().join("vulcan.sock");
    std::fs::write(&path, "stale").unwrap();
    let _bind = SocketBinder::bind(&path)
        .await
        .expect("must replace stale file");
    assert!(path.exists(), "new socket exists");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[tokio::test]
async fn socket_binder_refuses_live_socket() {
    // A live daemon's socket must NOT be unlinked by a second bind attempt.
    let dir = tempdir().unwrap();
    let path = dir.path().join("vulcan.sock");
    let _first = SocketBinder::bind(&path).await.expect("first bind OK");
    let second = SocketBinder::bind(&path).await;
    assert!(second.is_err(), "second bind on live socket must fail");
    let err = second.err().unwrap();
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
}
