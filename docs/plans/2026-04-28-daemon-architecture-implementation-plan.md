# Daemon Architecture Implementation Plan (YYC-266)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace per-invocation cold-start with a long-lived `vulcan daemon` that owns the Agent, CortexStore, SessionStore, LSP pool, tools, and hooks. All frontends (TUI, CLI, gateway) become thin clients over a Unix domain socket. Eliminates redb-lock conflicts and 3-5s cold-start. Phases through to multi-session and multi-agent collab.

**Architecture:** Daemon-skeleton-first sequencing. Slice 0 builds protocol/lifecycle scaffolding once; Slices 1-4 only add resource handlers and frontend rewires. Daemon-required (no in-process fallback) with `fork+exec` auto-start. JSON wire format with `session_id` from day 1. TUI ports bottom-up, one RPC at a time.

**Tech Stack:** Tokio (async runtime), `tokio::net::UnixListener`/`UnixStream`, `notify` (config watch), `nix` (fork/flock), `serde_json` (wire format), `assert_cmd` (e2e tests), existing `tempfile`.

**Design doc:** [`2026-04-28-daemon-architecture-design.md`](./2026-04-28-daemon-architecture-design.md) — read first. All locked decisions live there.

**Linear:** Parent epic YYC-266. Each slice gets a child issue (created during PR submission per "split issues per PR" rule).

---

## Current Progress (2026-04-30)

This implementation plan is the original YYC-266 daemon architecture plan. The current follow-on plan for runtime-resource-pool deepening is `docs/plans/2026-04-30-runtime-resource-pool-implementation-plan.md`.

Implemented and verified in the codebase:

- Slice 0 daemon skeleton: protocol envelope, length-delimited frames, PID/socket lifecycle, daemon subcommands, in-tree client auto-start, and config reload watcher.
- Slice 1 cortex daemon residence: daemon-owned cortex store for base cortex operations; direct transient redb opens removed from normal daemon cortex admin paths.
- Slice 2 daemon agent surface: daemon sessions own long-lived agents; `prompt.run`, `prompt.stream`, `prompt.cancel`, and `agent.*` handlers exist.
- Slice 3 multi-session/gateway foundation: `SessionMap`, session create/destroy/list, idle eviction, lane/session mapping, and runtime-pool-backed daemon agent construction exist.
- Slice 4 child-session/subagent foundation has been deepened through runtime-resource-pool work; delegated work is tracked with parent lineage and subagent origin.
- Cortex admin/storage fix from the runtime-resource-pool plan is implemented: stats, edge operations, delete, decay, search, traverse, and prompt management route through daemon-owned cortex storage.
- Slice 5 client transport multiplexing is implemented: the client transport has one read task keyed by request id, supports stream frames and `id: null` push frames, and the daemon server keeps reading the same socket while a stream is in flight by running per-request dispatch behind one serialized writer queue.
- Slice 6 gateway shared daemon client is implemented: gateway runtime owns one reusable daemon client, workers and slash-command handlers share it, and `DaemonLaneRouter` owns only lane/session mapping.
- Slice 7 child sessions for subagents is implemented for daemon-managed turns: live daemon session metadata records parent session id and lineage label, and daemon-managed `spawn_subagent` delegates to daemon child sessions instead of building direct child agents.

Known remaining work:

- Slice 7 hardening: add a provider-backed daemon integration test for a real prompt-triggered `spawn_subagent`; direct child-agent fallback remains only for non-daemon callers.
- Daemon config reload is still mostly a validated stub for full runtime reconfiguration; restarting the daemon is required for some resource shape changes.
- Some old plan bullets below are historical and have been superseded by the runtime-resource-pool plan and follow-up commits.

---

## How to use this plan

1. **Read the design doc first.** This plan assumes those decisions are settled.
2. **Slices land sequentially.** Slice 0 ships pure infra; only after it's green do you start Slice 1.
3. **Slice 0 and Slice 1 are detailed step-by-step.** Slices 2-4 are PR-level outlines; expand each into its own implementation plan after the prior slice ships (the structure changes after each landing as patterns settle).
4. **TDD discipline.** Every task is: failing test → minimal code → passing test → commit. Use `superpowers:test-driven-development` if you drift.
5. **Use `oo cargo build`/`oo cargo test`** to trim output tokens (per project memory).
6. **Commit per task.** No batching.

---

## Pre-flight: Branch + worktree

### Task -1: Create work branch

**Steps:**
- Branch name: `yyc-266-slice-0-daemon-skeleton`
- Use `superpowers:using-git-worktrees` to spin up isolated workdir.

```bash
git fetch origin
git worktree add -b yyc-266-slice-0-daemon-skeleton ../vulcan-yyc266 origin/main
cd ../vulcan-yyc266
```

---

# SLICE 0 — Daemon skeleton

**Goal:** Daemon binary subcommand + UnixListener + envelope + auto-start + lifecycle, with stub handlers only. Zero user-visible value. Headline test: cold CLI invocation auto-starts daemon, second CLI reuses it (single PID).

**Out of scope:** Any handler that touches CortexStore, SessionStore, or Agent. Those land in Slice 1+.

**Linear child issue:** YYC-266-A "Daemon skeleton + auto-start"

---

### Task 0.1: Add deps + `daemon` feature flag

**Files:**
- Modify: `Cargo.toml`

**Step 1: Edit Cargo.toml**

In `[features]` block, add:

```toml
# Daemon process + Unix socket frontend. Default-on; turn off only for
# embedded/no-IPC builds.
daemon = ["dep:notify", "dep:nix"]
```

Update `default = []` to `default = ["daemon"]`.

In `[dependencies]`, add (verify each is at crates.io latest before committing — see [feedback memory](../../../.claude/projects/-home-yycholla-vulcan/memory/feedback_latest_stable_deps.md)):

```toml
# File watcher for daemon config auto-reload (YYC-266 Slice 0). Cross-platform
# inotify/fseventsd/ReadDirectoryChangesW; debounces under the hood.
notify = { version = "...", optional = true }

# Unix-only primitives the daemon needs: fork+exec for auto-start,
# flock for PID file races, signal handling.
nix = { version = "...", optional = true, features = ["fs", "process", "signal"] }
```

In `[dev-dependencies]`, ensure these exist (add if missing):

```toml
assert_cmd = "..."
predicates = "..."
```

**Step 2: Verify it compiles**

```bash
oo cargo check --features daemon
```

Expected: clean.

**Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: add daemon feature flag and notify/nix deps for YYC-266 Slice 0"
```

---

### Task 0.2: Wire format types — `Request`, `Response`, `StreamFrame`, `Error`

**Files:**
- Create: `src/daemon/mod.rs` (with `#[cfg(feature = "daemon")]` gate)
- Create: `src/daemon/protocol.rs`
- Create: `src/daemon/protocol_tests.rs`

**Step 1: Write the failing test**

Create `src/daemon/protocol_tests.rs`:

```rust
use super::protocol::*;

#[test]
fn request_round_trips_through_json() {
    let req = Request {
        version: 1,
        id: "req-1".into(),
        session: "main".into(),
        method: "daemon.ping".into(),
        params: serde_json::json!({}),
    };
    let bytes = serde_json::to_vec(&req).unwrap();
    let parsed: Request = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed.id, "req-1");
    assert_eq!(parsed.method, "daemon.ping");
}

#[test]
fn response_with_error_serializes_with_null_result() {
    let resp = Response::error(
        "req-1".into(),
        ProtocolError {
            code: "VERSION_MISMATCH".into(),
            message: "client v2, daemon v1".into(),
            retryable: false,
        },
    );
    let v: serde_json::Value = serde_json::to_value(&resp).unwrap();
    assert_eq!(v["error"]["code"], "VERSION_MISMATCH");
    assert!(v["result"].is_null());
}

#[test]
fn version_mismatch_rejected_before_dispatch() {
    let req_v2 = serde_json::json!({
        "version": 2, "id": "x", "session": "main",
        "method": "daemon.ping", "params": {}
    });
    let bytes = serde_json::to_vec(&req_v2).unwrap();
    let result = parse_request_strict(&bytes);
    assert!(matches!(result, Err(ProtocolError { ref code, .. }) if code == "VERSION_MISMATCH"));
}

#[test]
fn stream_frame_text_chunk_round_trip() {
    let frame = StreamFrame {
        version: 1,
        id: Some("req-1".into()),
        stream: "text".into(),
        data: serde_json::json!({"chunk": "Hello"}),
    };
    let s = serde_json::to_string(&frame).unwrap();
    let parsed: StreamFrame = serde_json::from_str(&s).unwrap();
    assert_eq!(parsed.stream, "text");
    assert_eq!(parsed.data["chunk"], "Hello");
}
```

**Step 2: Run test to verify it fails**

```bash
oo cargo test --features daemon -p vulcan protocol -- --nocapture
```

Expected: FAIL — `protocol` module doesn't exist.

**Step 3: Implement protocol types**

Create `src/daemon/mod.rs`:

```rust
#![cfg(feature = "daemon")]

pub mod protocol;

#[cfg(test)]
mod protocol_tests;
```

Create `src/daemon/protocol.rs`:

```rust
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub version: u32,
    pub id: String,
    #[serde(default = "default_session")]
    pub session: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

fn default_session() -> String { "main".into() }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub version: u32,
    pub id: String,
    pub result: Option<serde_json::Value>,
    pub error: Option<ProtocolError>,
}

impl Response {
    pub fn ok(id: String, result: serde_json::Value) -> Self {
        Self { version: PROTOCOL_VERSION, id, result: Some(result), error: None }
    }
    pub fn error(id: String, err: ProtocolError) -> Self {
        Self { version: PROTOCOL_VERSION, id, result: None, error: Some(err) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ProtocolError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamFrame {
    pub version: u32,
    /// Request id this frame is part of, or None for out-of-band push frames.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub stream: String,
    pub data: serde_json::Value,
}

pub fn parse_request_strict(bytes: &[u8]) -> Result<Request, ProtocolError> {
    let req: Request = serde_json::from_slice(bytes).map_err(|e| ProtocolError {
        code: "INVALID_PARAMS".into(),
        message: format!("malformed request: {e}"),
        retryable: false,
    })?;
    if req.version != PROTOCOL_VERSION {
        return Err(ProtocolError {
            code: "VERSION_MISMATCH".into(),
            message: format!("client v{}, daemon v{PROTOCOL_VERSION}", req.version),
            retryable: false,
        });
    }
    Ok(req)
}
```

Add `mod daemon;` (with cfg gate) to `src/lib.rs` (or `src/main.rs` if no lib). Verify path.

**Step 4: Run test to verify it passes**

```bash
oo cargo test --features daemon -p vulcan protocol_tests
```

Expected: 4 passed.

**Step 5: Commit**

```bash
git add src/daemon/mod.rs src/daemon/protocol.rs src/daemon/protocol_tests.rs src/lib.rs
git commit -m "feat(daemon): wire protocol types — Request/Response/StreamFrame/Error"
```

---

### Task 0.3: Length-delimited frame I/O

**Files:**
- Modify: `src/daemon/protocol.rs` (add async read/write helpers)
- Modify: `src/daemon/protocol_tests.rs`

**Step 1: Failing test (frame round-trip over duplex pipe)**

Append to `protocol_tests.rs`:

```rust
use tokio::io::duplex;

#[tokio::test]
async fn frame_round_trip_over_duplex() {
    let (mut a, mut b) = duplex(4096);
    let req = Request {
        version: 1, id: "x".into(), session: "main".into(),
        method: "daemon.ping".into(), params: serde_json::json!({}),
    };
    super::protocol::write_request(&mut a, &req).await.unwrap();
    let got = super::protocol::read_request(&mut b).await.unwrap();
    assert_eq!(got.id, "x");
}

#[tokio::test]
async fn oversized_frame_rejected() {
    let (mut a, mut b) = duplex(8);
    // 5MB body header — exceeds MAX_FRAME_BYTES
    let huge_len: u32 = 5 * 1024 * 1024;
    use tokio::io::AsyncWriteExt;
    a.write_all(&huge_len.to_be_bytes()).await.unwrap();
    let result = super::protocol::read_request(&mut b).await;
    assert!(result.is_err());
}
```

**Step 2: Run — should fail (functions don't exist)**

```bash
oo cargo test --features daemon frame_round_trip
```

**Step 3: Implement frame helpers in `protocol.rs`**

```rust
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024; // 4 MiB

pub async fn write_request<W: AsyncWrite + Unpin>(w: &mut W, req: &Request) -> std::io::Result<()> {
    let body = serde_json::to_vec(req).map_err(|e| std::io::Error::other(e))?;
    write_frame_bytes(w, &body).await
}

pub async fn read_request<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Request> {
    let body = read_frame_bytes(r).await?;
    parse_request_strict(&body).map_err(|e| std::io::Error::other(e))
}

pub async fn write_frame_bytes<W: AsyncWrite + Unpin>(w: &mut W, body: &[u8]) -> std::io::Result<()> {
    let len: u32 = body.len().try_into().map_err(|_| std::io::Error::other("frame too large"))?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(body).await?;
    w.flush().await
}

pub async fn read_frame_bytes<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(std::io::Error::other(format!("frame size {len} > MAX_FRAME_BYTES")));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(body)
}

// Symmetric helpers for Response and StreamFrame
pub async fn write_response<W: AsyncWrite + Unpin>(w: &mut W, resp: &Response) -> std::io::Result<()> {
    let body = serde_json::to_vec(resp).map_err(std::io::Error::other)?;
    write_frame_bytes(w, &body).await
}

pub async fn write_stream_frame<W: AsyncWrite + Unpin>(w: &mut W, frame: &StreamFrame) -> std::io::Result<()> {
    let body = serde_json::to_vec(frame).map_err(std::io::Error::other)?;
    write_frame_bytes(w, &body).await
}
```

**Step 4: Run — passes**

```bash
oo cargo test --features daemon frame_round_trip
oo cargo test --features daemon oversized_frame_rejected
```

**Step 5: Commit**

```bash
git add src/daemon/protocol.rs src/daemon/protocol_tests.rs
git commit -m "feat(daemon): length-delimited frame I/O with 4MiB cap"
```

---

### Task 0.4: PID file lifecycle (`O_CREAT | O_EXCL` race)

**Files:**
- Create: `src/daemon/lifecycle.rs`
- Create: `src/daemon/lifecycle_tests.rs`

**Step 1: Failing test**

`src/daemon/lifecycle_tests.rs`:

```rust
use super::lifecycle::*;
use tempfile::tempdir;

#[test]
fn pid_file_create_excl_rejects_second_writer() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("daemon.pid");
    let _first = PidFile::acquire(&path).expect("first acquire OK");
    let second = PidFile::acquire(&path);
    assert!(second.is_err(), "second acquire must fail");
}

#[test]
fn pid_file_released_on_drop() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("daemon.pid");
    {
        let _f = PidFile::acquire(&path).unwrap();
    } // dropped here
    let again = PidFile::acquire(&path);
    assert!(again.is_ok(), "drop must release");
}

#[test]
fn pid_file_detects_stale_pid() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("daemon.pid");
    // Write a fake PID that's almost certainly dead.
    std::fs::write(&path, "999999\n").unwrap();
    // Stale-aware acquire should succeed by overwriting.
    let _f = PidFile::acquire_or_replace_stale(&path).expect("stale PID overwritten");
}
```

**Step 2: Run — fails (module doesn't exist)**

```bash
oo cargo test --features daemon lifecycle_tests
```

**Step 3: Implement `lifecycle.rs`**

```rust
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub struct PidFile {
    path: PathBuf,
    _file: File,
}

impl PidFile {
    pub fn acquire(path: &Path) -> std::io::Result<Self> {
        let mut file = OpenOptions::new()
            .read(true).write(true).create_new(true)
            .mode(0o600).open(path)?;
        write!(file, "{}\n", std::process::id())?;
        Ok(Self { path: path.to_path_buf(), _file: file })
    }

    pub fn acquire_or_replace_stale(path: &Path) -> std::io::Result<Self> {
        match Self::acquire(path) {
            Ok(f) => Ok(f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Read existing PID and probe.
                let mut s = String::new();
                File::open(path)?.read_to_string(&mut s)?;
                let pid: i32 = s.trim().parse()
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

fn is_alive(pid: i32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid), None).is_ok()
}
```

Wire into `mod.rs`: `pub mod lifecycle;` and `mod lifecycle_tests;`.

**Step 4: Run — passes**

```bash
oo cargo test --features daemon lifecycle
```

**Step 5: Commit**

```bash
git add src/daemon/lifecycle.rs src/daemon/lifecycle_tests.rs src/daemon/mod.rs
git commit -m "feat(daemon): PID file with O_CREAT|O_EXCL acquire and stale-PID detection"
```

---

### Task 0.5: Socket bind + cleanup

**Files:**
- Modify: `src/daemon/lifecycle.rs`
- Modify: `src/daemon/lifecycle_tests.rs`

**Step 1: Failing test**

```rust
#[tokio::test]
async fn socket_binder_creates_0600_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let path = dir.path().join("vulcan.sock");
    let _bind = SocketBinder::bind(&path).await.unwrap();
    let meta = std::fs::metadata(&path).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
}

#[tokio::test]
async fn socket_binder_removes_stale() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vulcan.sock");
    // Stale file that isn't a live socket
    std::fs::write(&path, "stale").unwrap();
    let _bind = SocketBinder::bind(&path).await.expect("must replace stale");
    assert!(path.exists(), "new socket exists");
}

#[tokio::test]
async fn socket_dropped_removes_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("vulcan.sock");
    {
        let _b = SocketBinder::bind(&path).await.unwrap();
    }
    assert!(!path.exists(), "drop must clean up");
}
```

**Step 2: Run — fails**

**Step 3: Implement `SocketBinder` in `lifecycle.rs`**

```rust
use tokio::net::UnixListener;

pub struct SocketBinder {
    pub listener: UnixListener,
    path: PathBuf,
}

impl SocketBinder {
    pub async fn bind(path: &Path) -> std::io::Result<Self> {
        // Probe: if a socket exists and accepts connections, refuse.
        if path.exists() {
            match tokio::net::UnixStream::connect(path).await {
                Ok(_) => return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "socket already in use by live daemon",
                )),
                Err(_) => {
                    std::fs::remove_file(path).ok();
                }
            }
        }
        let listener = UnixListener::bind(path)?;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(path, perms)?;
        Ok(Self { listener, path: path.to_path_buf() })
    }
}

impl Drop for SocketBinder {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
```

**Step 4: Run — passes**

**Step 5: Commit**

```bash
git add src/daemon/lifecycle.rs src/daemon/lifecycle_tests.rs
git commit -m "feat(daemon): UnixListener bind with 0600 perms and stale-socket cleanup"
```

---

### Task 0.6: Method dispatcher with stub `daemon.ping`

**Files:**
- Create: `src/daemon/dispatch.rs`
- Create: `src/daemon/handlers/mod.rs`
- Create: `src/daemon/handlers/daemon_ops.rs`
- Modify: `src/daemon/mod.rs`

**Step 1: Failing test (in `dispatch.rs` itself or new test mod)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::protocol::*;

    fn ping_req() -> Request {
        Request {
            version: 1, id: "p1".into(), session: "main".into(),
            method: "daemon.ping".into(), params: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn ping_dispatches_to_daemon_ops() {
        let dispatcher = Dispatcher::new(test_state());
        let resp = dispatcher.dispatch(ping_req()).await;
        let result = resp.result.expect("ping ok");
        assert!(result["pong"].as_bool().unwrap_or(false));
        assert!(result["pid"].is_number());
    }

    #[tokio::test]
    async fn unknown_method_returns_error() {
        let dispatcher = Dispatcher::new(test_state());
        let mut req = ping_req();
        req.method = "does.not.exist".into();
        let resp = dispatcher.dispatch(req).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, "UNKNOWN_METHOD");
    }

    fn test_state() -> std::sync::Arc<crate::daemon::DaemonState> {
        std::sync::Arc::new(crate::daemon::DaemonState::for_tests())
    }
}
```

**Step 2: Run — fails**

**Step 3: Implement**

`src/daemon/handlers/mod.rs`:
```rust
pub mod daemon_ops;
```

`src/daemon/handlers/daemon_ops.rs`:
```rust
use serde_json::json;
use crate::daemon::DaemonState;
use crate::daemon::protocol::{ProtocolError, Response};

pub async fn ping(state: &DaemonState, id: String) -> Response {
    Response::ok(id, json!({
        "pong": true,
        "pid": std::process::id(),
        "uptime_secs": state.uptime_secs(),
    }))
}

pub async fn shutdown(state: &DaemonState, id: String, _force: bool) -> Response {
    state.signal_shutdown();
    Response::ok(id, json!({"ok": true}))
}

pub async fn reload(state: &DaemonState, id: String) -> Response {
    state.queue_reload();
    Response::ok(id, json!({"ok": true}))
}

pub async fn status(state: &DaemonState, id: String) -> Response {
    Response::ok(id, json!({
        "pid": std::process::id(),
        "uptime_secs": state.uptime_secs(),
        "sessions": state.session_descriptors(), // Vec<{id, last_activity, in_flight}>
    }))
}
```

`src/daemon/dispatch.rs`:
```rust
use std::sync::Arc;
use crate::daemon::DaemonState;
use crate::daemon::protocol::{ProtocolError, Request, Response};

pub struct Dispatcher {
    state: Arc<DaemonState>,
}

impl Dispatcher {
    pub fn new(state: Arc<DaemonState>) -> Self { Self { state } }

    pub async fn dispatch(&self, req: Request) -> Response {
        use crate::daemon::handlers::daemon_ops as d;
        match req.method.as_str() {
            "daemon.ping" => d::ping(&self.state, req.id).await,
            "daemon.shutdown" => {
                let force = req.params.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
                d::shutdown(&self.state, req.id, force).await
            }
            "daemon.reload" => d::reload(&self.state, req.id).await,
            "daemon.status" => d::status(&self.state, req.id).await,
            other => Response::error(req.id, ProtocolError {
                code: "UNKNOWN_METHOD".into(),
                message: format!("unknown method: {other}"),
                retryable: false,
            }),
        }
    }
}

#[cfg(test)]
mod tests { /* moved from above */ }
```

`src/daemon/mod.rs` add:
```rust
pub mod dispatch;
pub mod handlers;
mod state;
pub use state::DaemonState;
```

`src/daemon/state.rs` (minimal):
```rust
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Notify;

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

    #[cfg(test)]
    pub fn for_tests() -> Self { Self::new() }

    pub fn uptime_secs(&self) -> u64 { self.started_at.elapsed().as_secs() }
    pub fn signal_shutdown(&self) { self.shutdown.notify_waiters(); }
    pub fn shutdown_signal(&self) -> Arc<Notify> { self.shutdown.clone() }
    pub fn queue_reload(&self) { self.reload.notify_waiters(); }
    pub fn reload_signal(&self) -> Arc<Notify> { self.reload.clone() }
    pub fn session_descriptors(&self) -> Vec<serde_json::Value> { vec![] } // Slice 0 stub
}
```

**Step 4: Run — passes**

```bash
oo cargo test --features daemon dispatch::tests
```

**Step 5: Commit**

```bash
git add src/daemon/dispatch.rs src/daemon/handlers/ src/daemon/state.rs src/daemon/mod.rs
git commit -m "feat(daemon): method dispatcher with daemon.{ping,shutdown,reload,status} stubs"
```

---

### Task 0.7: Server accept loop

**Files:**
- Create: `src/daemon/server.rs`
- Modify: `src/daemon/mod.rs`

**Step 1: Failing test (in `server.rs`)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::protocol::*;
    use tempfile::tempdir;
    use tokio::net::UnixStream;

    #[tokio::test]
    async fn server_responds_to_ping() {
        let dir = tempdir().unwrap();
        let sock_path = dir.path().join("vulcan.sock");

        let state = std::sync::Arc::new(crate::daemon::DaemonState::for_tests());
        let server = Server::bind(&sock_path, state.clone()).await.unwrap();
        tokio::spawn(server.run());

        let mut client = UnixStream::connect(&sock_path).await.unwrap();
        let req = Request {
            version: 1, id: "p1".into(), session: "main".into(),
            method: "daemon.ping".into(), params: serde_json::json!({}),
        };
        write_request(&mut client, &req).await.unwrap();

        let body = read_frame_bytes(&mut client).await.unwrap();
        let resp: Response = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp.id, "p1");
        assert!(resp.result.unwrap()["pong"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn server_handles_concurrent_clients() {
        // Spawn server, 8 clients each send ping, all must succeed
        // (covers per-conn task spawning)
        // ... implement same pattern as above with N spawned tasks
    }
}
```

**Step 2: Run — fails**

**Step 3: Implement**

```rust
use std::path::Path;
use std::sync::Arc;
use tokio::net::UnixListener;
use crate::daemon::dispatch::Dispatcher;
use crate::daemon::lifecycle::SocketBinder;
use crate::daemon::protocol::{read_frame_bytes, write_response, parse_request_strict, ProtocolError, Response};
use crate::daemon::DaemonState;

pub struct Server {
    binder: SocketBinder,
    dispatcher: Arc<Dispatcher>,
    state: Arc<DaemonState>,
}

impl Server {
    pub async fn bind(path: &Path, state: Arc<DaemonState>) -> std::io::Result<Self> {
        let binder = SocketBinder::bind(path).await?;
        let dispatcher = Arc::new(Dispatcher::new(state.clone()));
        Ok(Self { binder, dispatcher, state })
    }

    pub async fn run(self) {
        let shutdown = self.state.shutdown_signal();
        loop {
            tokio::select! {
                _ = shutdown.notified() => {
                    tracing::info!("daemon shutting down");
                    return;
                }
                accept = self.binder.listener.accept() => {
                    match accept {
                        Ok((stream, _)) => {
                            let dispatcher = self.dispatcher.clone();
                            tokio::spawn(handle_connection(stream, dispatcher));
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "accept failed");
                        }
                    }
                }
            }
        }
    }
}

async fn handle_connection(mut stream: tokio::net::UnixStream, dispatcher: Arc<Dispatcher>) {
    loop {
        let body = match read_frame_bytes(&mut stream).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return,
            Err(e) => {
                tracing::warn!(error = %e, "read failed");
                return;
            }
        };

        let req = match parse_request_strict(&body) {
            Ok(r) => r,
            Err(e) => {
                let resp = Response::error("unknown".into(), e);
                let _ = write_response(&mut stream, &resp).await;
                continue;
            }
        };

        let resp = dispatcher.dispatch(req).await;
        if let Err(e) = write_response(&mut stream, &resp).await {
            tracing::warn!(error = %e, "write failed");
            return;
        }
    }
}
```

Add `pub mod server;` to `daemon/mod.rs`.

**Step 4: Run — passes**

```bash
oo cargo test --features daemon server::tests
```

**Step 5: Commit**

```bash
git add src/daemon/server.rs src/daemon/mod.rs
git commit -m "feat(daemon): UnixListener accept loop with per-conn dispatch"
```

---

### Task 0.8: `vulcan daemon` subcommand

**Files:**
- Modify: `src/cli.rs` (add `Daemon` subcommand)
- Create: `src/daemon/cli.rs` (subcommand handler)
- Modify: `src/main.rs` (route subcommand)

**Step 1: Failing test (e2e via assert_cmd)**

`tests/daemon_e2e.rs`:
```rust
use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn vulcan_with_home(home: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("vulcan").unwrap();
    c.env("VULCAN_HOME", home);
    c.env("RUST_LOG", "warn");
    c
}

#[test]
fn daemon_start_then_status_then_stop() {
    let dir = tempdir().unwrap();
    // Detached start
    vulcan_with_home(dir.path())
        .args(&["daemon", "start", "--detach"])
        .assert().success();

    // Wait briefly for socket
    std::thread::sleep(std::time::Duration::from_millis(500));

    vulcan_with_home(dir.path())
        .args(&["daemon", "status"])
        .assert().success()
        .stdout(predicate::str::contains("pid"));

    vulcan_with_home(dir.path())
        .args(&["daemon", "stop"])
        .assert().success();
}
```

**Step 2: Run — fails (Daemon subcommand doesn't exist)**

```bash
oo cargo test --features daemon --test daemon_e2e
```

**Step 3: Implement**

`src/cli.rs` add:
```rust
#[cfg(feature = "daemon")]
#[derive(Subcommand)]
pub enum DaemonAction {
    Start {
        #[arg(long)]
        detach: bool,
    },
    Stop {
        #[arg(long)]
        force: bool,
    },
    Status,
    Reload,
    Install {
        #[arg(long)]
        systemd: bool,
    },
}
```

In top-level `Commands` enum:
```rust
#[cfg(feature = "daemon")]
Daemon {
    #[command(subcommand)]
    action: DaemonAction,
},
```

`src/daemon/cli.rs`:
```rust
use std::path::PathBuf;
use std::sync::Arc;
use crate::config::vulcan_home;
use crate::daemon::{server::Server, state::DaemonState, lifecycle::PidFile};

pub async fn run(action: crate::cli::DaemonAction) -> anyhow::Result<()> {
    match action {
        crate::cli::DaemonAction::Start { detach } => start(detach).await,
        crate::cli::DaemonAction::Stop { force } => stop(force).await,
        crate::cli::DaemonAction::Status => status().await,
        crate::cli::DaemonAction::Reload => reload().await,
        crate::cli::DaemonAction::Install { systemd } => install(systemd).await,
    }
}

async fn start(detach: bool) -> anyhow::Result<()> {
    let home = vulcan_home();
    std::fs::create_dir_all(&home)?;
    let pid_path = home.join("daemon.pid");
    let sock_path = home.join("vulcan.sock");

    if detach {
        // fork+exec ourselves without --detach
        return spawn_detached(&sock_path).await;
    }

    let _pidfile = PidFile::acquire_or_replace_stale(&pid_path)?;
    let state = Arc::new(DaemonState::new());
    let server = Server::bind(&sock_path, state.clone()).await?;

    // Install signal handlers
    let shutdown = state.shutdown_signal();
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate()).unwrap();
        let mut sigint = signal(SignalKind::interrupt()).unwrap();
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = sigint.recv() => {}
        }
        shutdown.notify_waiters();
    });

    server.run().await;
    Ok(())
}

async fn spawn_detached(sock_path: &PathBuf) -> anyhow::Result<()> {
    use std::process::Command;
    let exe = std::env::current_exe()?;
    Command::new(&exe).args(&["daemon", "start"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // Poll for socket up to 5s
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if tokio::net::UnixStream::connect(sock_path).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    anyhow::bail!("daemon failed to come up within 5s")
}

async fn stop(_force: bool) -> anyhow::Result<()> {
    // Connect and send daemon.shutdown
    let sock = vulcan_home().join("vulcan.sock");
    let mut s = tokio::net::UnixStream::connect(&sock).await?;
    use crate::daemon::protocol::*;
    let req = Request {
        version: 1, id: "stop-1".into(), session: "main".into(),
        method: "daemon.shutdown".into(), params: serde_json::json!({"force": _force}),
    };
    write_request(&mut s, &req).await?;
    let _ = read_frame_bytes(&mut s).await?;
    Ok(())
}

async fn status() -> anyhow::Result<()> {
    let sock = vulcan_home().join("vulcan.sock");
    let mut s = tokio::net::UnixStream::connect(&sock).await?;
    use crate::daemon::protocol::*;
    let req = Request {
        version: 1, id: "stat-1".into(), session: "main".into(),
        method: "daemon.status".into(), params: serde_json::json!({}),
    };
    write_request(&mut s, &req).await?;
    let body = read_frame_bytes(&mut s).await?;
    let resp: Response = serde_json::from_slice(&body)?;
    println!("{}", serde_json::to_string_pretty(&resp.result.unwrap_or_default())?);
    Ok(())
}

async fn reload() -> anyhow::Result<()> { /* analogous */ Ok(()) }
async fn install(_systemd: bool) -> anyhow::Result<()> { /* deferred to 0.12 */ Ok(()) }
```

`src/main.rs` add:
```rust
#[cfg(feature = "daemon")]
Commands::Daemon { action } => crate::daemon::cli::run(action).await?,
```

**Step 4: Run e2e — passes**

```bash
oo cargo test --features daemon --test daemon_e2e
```

**Step 5: Commit**

```bash
git add src/cli.rs src/daemon/cli.rs src/daemon/mod.rs src/main.rs tests/daemon_e2e.rs
git commit -m "feat(daemon): vulcan daemon {start,stop,status} subcommand"
```

---

### Task 0.9: SessionMap with `"main"` pre-created

**Files:**
- Create: `src/daemon/session.rs`
- Modify: `src/daemon/state.rs` (hold SessionMap)
- Test: `src/daemon/session.rs` `#[cfg(test)] mod tests`

**Step 1: Failing test**

```rust
#[tokio::test]
async fn session_map_has_main_at_init() {
    let map = SessionMap::with_main();
    assert!(map.get("main").is_some());
}

#[tokio::test]
async fn session_map_create_and_destroy() {
    let map = SessionMap::with_main();
    let id = map.create_named("foo").await.unwrap();
    assert_eq!(id, "foo");
    assert!(map.get("foo").is_some());
    map.destroy("foo").await;
    assert!(map.get("foo").is_none());
}

#[tokio::test]
async fn cannot_destroy_main() {
    let map = SessionMap::with_main();
    let res = map.destroy_checked("main").await;
    assert!(res.is_err(), "main is undeletable");
}
```

**Step 2: Run — fails**

**Step 3: Implement**

```rust
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use parking_lot::RwLock;
use tokio_util::sync::CancellationToken;

pub struct SessionState {
    pub id: String,
    pub created_at: Instant,
    pub last_activity: parking_lot::Mutex<Instant>,
    pub in_flight: parking_lot::Mutex<bool>,
    pub cancel: CancellationToken,
    // Slice 1+ adds: pub agent: Agent, pub audit_buf: ...
}

impl SessionState {
    pub fn new(id: String) -> Self {
        Self {
            id,
            created_at: Instant::now(),
            last_activity: Instant::now().into(),
            in_flight: false.into(),
            cancel: CancellationToken::new(),
        }
    }
}

pub struct SessionMap {
    inner: RwLock<HashMap<String, Arc<SessionState>>>,
}

impl SessionMap {
    pub fn with_main() -> Self {
        let mut m = HashMap::new();
        m.insert("main".into(), Arc::new(SessionState::new("main".into())));
        Self { inner: RwLock::new(m) }
    }

    pub fn get(&self, id: &str) -> Option<Arc<SessionState>> {
        self.inner.read().get(id).cloned()
    }

    pub async fn create_named(&self, id: &str) -> anyhow::Result<String> {
        let mut g = self.inner.write();
        if g.contains_key(id) { anyhow::bail!("session exists: {id}"); }
        g.insert(id.into(), Arc::new(SessionState::new(id.into())));
        Ok(id.into())
    }

    pub async fn destroy(&self, id: &str) {
        self.inner.write().remove(id);
    }

    pub async fn destroy_checked(&self, id: &str) -> anyhow::Result<()> {
        if id == "main" { anyhow::bail!("cannot destroy 'main'"); }
        self.destroy(id).await;
        Ok(())
    }

    pub fn descriptors(&self) -> Vec<serde_json::Value> {
        self.inner.read().values().map(|s| {
            serde_json::json!({
                "id": s.id,
                "in_flight": *s.in_flight.lock(),
                "last_activity_secs_ago": s.last_activity.lock().elapsed().as_secs(),
            })
        }).collect()
    }
}
```

Add `tokio-util = { version = "...", features = ["rt"] }` to `Cargo.toml` if not present.

Modify `DaemonState` to own `Arc<SessionMap>`. Update `session_descriptors()` to delegate.

**Step 4: Run — passes**

**Step 5: Commit**

```bash
git add src/daemon/session.rs src/daemon/state.rs Cargo.toml Cargo.lock
git commit -m "feat(daemon): SessionMap with default 'main' session"
```

---

### Task 0.10: Config watcher with idle-deferred reload

**Files:**
- Create: `src/daemon/config_watch.rs`
- Test: same file

**Step 1: Failing test**

```rust
#[tokio::test]
async fn reload_deferred_while_session_in_flight() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("config.toml");
    std::fs::write(&cfg_path, "key = 1").unwrap();

    let state = Arc::new(DaemonState::new());
    let main = state.sessions().get("main").unwrap();
    *main.in_flight.lock() = true;

    let (events_tx, _events_rx) = tokio::sync::broadcast::channel(8);
    let watcher = ConfigWatcher::start(&cfg_path, state.clone(), events_tx).unwrap();

    // Edit
    std::fs::write(&cfg_path, "key = 2").unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Reload should NOT have fired (in_flight = true)
    assert_eq!(watcher.reloads_applied(), 0);

    // Clear in_flight
    *main.in_flight.lock() = false;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    assert_eq!(watcher.reloads_applied(), 1);
}
```

**Step 2: Run — fails**

**Step 3: Implement**

```rust
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use notify::{RecursiveMode, Watcher};
use tokio::sync::broadcast;
use crate::daemon::DaemonState;

pub struct ConfigWatcher {
    reloads_applied: Arc<AtomicU64>,
    _watcher: notify::RecommendedWatcher,
}

impl ConfigWatcher {
    pub fn start(
        config_path: &Path,
        state: Arc<DaemonState>,
        events_tx: broadcast::Sender<ReloadEvent>,
    ) -> notify::Result<Self> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if matches!(ev.kind, notify::EventKind::Modify(_) | notify::EventKind::Create(_)) {
                    let _ = tx.send(());
                }
            }
        })?;
        watcher.watch(config_path, RecursiveMode::NonRecursive)?;

        let reloads_applied = Arc::new(AtomicU64::new(0));
        let reloads_clone = reloads_applied.clone();
        let state_clone = state.clone();
        let cfg_path = config_path.to_path_buf();

        tokio::spawn(async move {
            while rx.recv().await.is_some() {
                // Drain bursts (notify often emits 2-3 events per save)
                while rx.try_recv().is_ok() {}
                // Wait for idle: poll until no session is in_flight
                loop {
                    if !state_clone.sessions().any_in_flight() {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                match crate::config::Config::load_from(&cfg_path) {
                    Ok(new_cfg) => {
                        state_clone.apply_config(new_cfg).await;
                        reloads_clone.fetch_add(1, Ordering::SeqCst);
                        let _ = events_tx.send(ReloadEvent::Applied);
                    }
                    Err(e) => {
                        let _ = events_tx.send(ReloadEvent::Failed(e.to_string()));
                    }
                }
            }
        });

        Ok(Self { reloads_applied, _watcher: watcher })
    }

    pub fn reloads_applied(&self) -> u64 {
        self.reloads_applied.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone)]
pub enum ReloadEvent {
    Applied,
    Failed(String),
}
```

Add helper on `SessionMap`: `pub fn any_in_flight(&self) -> bool { self.inner.read().values().any(|s| *s.in_flight.lock()) }`.

Add `apply_config` stub on `DaemonState` (full impl in Slice 2 when Agent exists).

**Step 4: Run — passes**

**Step 5: Commit**

```bash
git add src/daemon/config_watch.rs src/daemon/session.rs src/daemon/state.rs
git commit -m "feat(daemon): notify-based config watcher with idle-deferred reload"
```

---

### Task 0.11: `vulcan-client` (in-tree at `src/client/`) — auto-start

**Note:** Brainstorm doc said `crates/vulcan-client/` but in-tree `src/client/` is simpler — the binary is `vulcan` and all frontends are subcommands. Revisit only if a separate consumer ever appears.

**Files:**
- Create: `src/client/mod.rs`
- Create: `src/client/transport.rs`
- Create: `src/client/auto_start.rs`
- Test: `tests/client_autostart.rs`

**Step 1: Failing test**

```rust
use assert_cmd::Command;
use tempfile::tempdir;

#[test]
fn cold_invocation_autostarts_daemon() {
    let dir = tempdir().unwrap();
    let mut c = Command::cargo_bin("vulcan").unwrap();
    c.env("VULCAN_HOME", dir.path());
    // Hidden subcommand for tests: vulcan __ping (just calls daemon.ping)
    c.args(&["__ping"]).assert().success()
        .stdout(predicates::str::contains("pong"));

    // Daemon should be running now
    assert!(dir.path().join("vulcan.sock").exists());
}

#[test]
fn second_invocation_reuses_daemon() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("vulcan").unwrap()
        .env("VULCAN_HOME", dir.path()).args(&["__ping"])
        .assert().success();

    // Read PID after first invocation
    let pid1 = std::fs::read_to_string(dir.path().join("daemon.pid")).unwrap();

    Command::cargo_bin("vulcan").unwrap()
        .env("VULCAN_HOME", dir.path()).args(&["__ping"])
        .assert().success();

    let pid2 = std::fs::read_to_string(dir.path().join("daemon.pid")).unwrap();
    assert_eq!(pid1, pid2, "second invocation must reuse daemon (same PID)");
}

#[test]
fn autostart_race_settles_to_one_daemon() {
    let dir = tempdir().unwrap();
    let mut handles = vec![];
    for _ in 0..4 {
        let p = dir.path().to_path_buf();
        handles.push(std::thread::spawn(move || {
            Command::cargo_bin("vulcan").unwrap()
                .env("VULCAN_HOME", &p).args(&["__ping"])
                .assert().success();
        }));
    }
    for h in handles { h.join().unwrap(); }

    // Exactly one daemon process
    let pid = std::fs::read_to_string(dir.path().join("daemon.pid")).unwrap();
    let pid: i32 = pid.trim().parse().unwrap();
    assert!(pid > 0);
}
```

**Step 2: Run — fails**

**Step 3: Implement**

`src/client/transport.rs`:
```rust
use std::path::Path;
use tokio::net::UnixStream;
use crate::daemon::protocol::{read_frame_bytes, write_request, Request, Response};

pub struct Transport {
    stream: UnixStream,
}

impl Transport {
    pub async fn connect(path: &Path) -> std::io::Result<Self> {
        let stream = UnixStream::connect(path).await?;
        Ok(Self { stream })
    }

    pub async fn call(&mut self, req: Request) -> anyhow::Result<Response> {
        write_request(&mut self.stream, &req).await?;
        let body = read_frame_bytes(&mut self.stream).await?;
        Ok(serde_json::from_slice(&body)?)
    }
}
```

`src/client/auto_start.rs`:
```rust
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub async fn ensure_daemon(sock_path: &Path) -> anyhow::Result<()> {
    if can_connect(sock_path).await { return Ok(()); }

    // Stale socket?
    if sock_path.exists() {
        std::fs::remove_file(sock_path).ok();
    }

    let exe = std::env::current_exe()?;
    let _child = std::process::Command::new(&exe)
        .args(&["daemon", "start"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if can_connect(sock_path).await { return Ok(()); }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("daemon did not come up within 5s")
}

async fn can_connect(path: &Path) -> bool {
    tokio::net::UnixStream::connect(path).await.is_ok()
}
```

`src/client/mod.rs`:
```rust
pub mod transport;
pub mod auto_start;

use crate::config::vulcan_home;
use crate::daemon::protocol::Request;

pub struct Client {
    transport: transport::Transport,
}

impl Client {
    pub async fn connect_or_autostart() -> anyhow::Result<Self> {
        let sock = vulcan_home().join("vulcan.sock");
        auto_start::ensure_daemon(&sock).await?;
        let transport = transport::Transport::connect(&sock).await?;
        Ok(Self { transport })
    }

    pub async fn call(&mut self, method: &str, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let req = Request {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            session: "main".into(),
            method: method.into(),
            params,
        };
        let resp = self.transport.call(req).await?;
        if let Some(err) = resp.error {
            anyhow::bail!("{}: {}", err.code, err.message);
        }
        Ok(resp.result.unwrap_or_default())
    }
}
```

Add hidden `__ping` subcommand to `cli.rs` (gated, dev-only) that calls `Client::connect_or_autostart` and prints the result.

**Step 4: Run — passes**

```bash
oo cargo test --features daemon --test client_autostart
```

**Step 5: Commit**

```bash
git add src/client/ src/cli.rs src/main.rs tests/client_autostart.rs
git commit -m "feat(client): connect_or_autostart with stale-socket cleanup"
```

---

### Task 0.12: `daemon install --systemd`

**Files:**
- Create: `src/daemon/install.rs`
- Modify: `src/daemon/cli.rs` (wire `install`)

**Step 1: Failing test**

```rust
#[test]
fn install_writes_systemd_unit() {
    let dir = tempdir().unwrap();
    install_systemd(dir.path()).unwrap();
    let unit = dir.path().join("systemd/user/vulcan.service");
    let content = std::fs::read_to_string(&unit).unwrap();
    assert!(content.contains("[Service]"));
    assert!(content.contains("ExecStart="));
    assert!(content.contains("Restart=on-failure"));
}
```

**Step 2: Run — fails**

**Step 3: Implement**

```rust
use std::path::Path;

pub fn install_systemd(config_home: &Path) -> std::io::Result<()> {
    let unit_dir = config_home.join("systemd/user");
    std::fs::create_dir_all(&unit_dir)?;
    let exe = std::env::current_exe()?;
    let unit = format!(
        r#"[Unit]
Description=Vulcan AI agent daemon
After=network.target

[Service]
Type=simple
ExecStart={exe} daemon start
Restart=on-failure
RestartSec=2s

[Install]
WantedBy=default.target
"#,
        exe = exe.display(),
    );
    std::fs::write(unit_dir.join("vulcan.service"), unit)
}
```

In `cli.rs install()` action: resolve `$XDG_CONFIG_HOME` (fallback `$HOME/.config`) and call `install_systemd`.

**Step 4: Run — passes**

**Step 5: Commit**

```bash
git add src/daemon/install.rs src/daemon/cli.rs src/daemon/mod.rs
git commit -m "feat(daemon): vulcan daemon install --systemd writes user unit"
```

---

### Task 0.13: 0600 perms verification test

**Files:**
- Test only: `tests/daemon_e2e.rs` (extend)

```rust
#[test]
fn socket_perms_are_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    Command::cargo_bin("vulcan").unwrap()
        .env("VULCAN_HOME", dir.path()).args(&["__ping"])
        .assert().success();

    let sock = dir.path().join("vulcan.sock");
    let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}
```

Commit:
```bash
git add tests/daemon_e2e.rs
git commit -m "test(daemon): assert socket perms are 0600"
```

---

### Task 0.14: Slice 0 acceptance + PR

**Manual verification:**

```bash
oo cargo build --features daemon --release
target/release/vulcan daemon start --detach
target/release/vulcan daemon status   # prints JSON with pid + uptime
target/release/vulcan daemon stop
ls ~/.vulcan/vulcan.sock              # gone
```

**Test gate:**

```bash
oo cargo test --features daemon
```

All daemon e2e + unit tests green.

**PR:**

Use `superpowers:finishing-a-development-branch`. Create Linear child issue YYC-266-A. PR title: `feat(daemon): YYC-266 Slice 0 — daemon skeleton + auto-start`. Body links the design doc.

---

# SLICE 1 — Cortex daemon-resident

**Goal:** `CortexStore` + `SessionStore` move into daemon; cortex CLI subcommands and `vulcan search` route through the daemon. Eliminates redb-lock conflict.

**Linear child issue:** YYC-266-B "Cortex moves to daemon"

---

### Task 1.1: SharedResources struct + daemon startup wiring

**Files:**
- Create: `src/daemon/resources.rs`
- Modify: `src/daemon/state.rs`
- Modify: `src/daemon/cli.rs`

**Steps:**
1. Define `SharedResources { cortex: Arc<CortexStore>, sessions_db: Arc<SessionStore> }`.
2. Open both in `start()` after PID file, before listener bind.
3. Pass to `DaemonState::new(resources)`.
4. Test: daemon startup with `cortex.enabled = false` skips opening cortex.
5. Test: daemon startup with cortex configured opens it (verify `cortex.redb` file appears in tempdir).

Commit: `feat(daemon): open CortexStore + SessionStore as shared resources at boot`.

---

### Task 1.2: `cortex.store` + `cortex.search` handlers (TDD)

**Files:**
- Create: `src/daemon/handlers/cortex.rs`
- Modify: `src/daemon/dispatch.rs`

**Steps:**
1. Test: client calls `cortex.store {text: "rust is great", importance: 0.8}` → returns `node_id`.
2. Test: client calls `cortex.search {query: "rust", limit: 5}` → returns at least the stored node.
3. Implement `store` and `search` handlers; wire dispatcher.
4. Add `Client::call` ergonomics for cortex namespace.
5. Headline regression test: spawn TUI session simulator (acquires SessionMap entry, sets `in_flight=true`), concurrent client calls `cortex.search` — both succeed.

Commit per sub-step.

---

### Task 1.3: All remaining cortex methods

Iterate per method: `stats`, `recall`, `seed`, `edges_from`, `edges_to`, `delete_edge`, `update_edge_weight`, `run_decay`, `prompt.{create,get,list,set,remove,performance}`, `agent.{list,bind,unbind,select}`, `observe`.

For each:
1. Failing client-call test
2. Handler impl
3. Dispatcher wire
4. Commit `feat(daemon/cortex): add cortex.<method>`

---

### Task 1.4: Rewire `src/cli_cortex.rs`

**Files:**
- Modify: `src/cli_cortex.rs`

**Steps:**
1. Replace direct `CortexStore::try_open()` with `Client::connect_or_autostart()`.
2. Each subcommand becomes a `client.call("cortex.X", params).await?` then prints result.
3. Test: `vulcan cortex search "foo"` end-to-end via assert_cmd.
4. Test: `vulcan cortex stats` returns under 100ms with 1k seeded nodes (was O(N)).

Commit: `refactor(cli): cortex subcommands route through daemon`.

---

### Task 1.5: Rewire `vulcan search` (FTS5)

**Files:**
- Modify: `src/main.rs` `Search` arm
- Modify: `src/daemon/handlers/session.rs` (new) — implement `session.search`

**Steps:**
1. Add `session.search { query, limit }` handler that wraps existing `SessionStore::search`.
2. Test via client.
3. Modify `Search` CLI arm to use client.
4. e2e test: `vulcan search "hello"` returns hits without opening SQLite directly.

Commit: `refactor(cli): vulcan search routes through daemon`.

---

### Task 1.6: Delete transient redb hack

**Files:**
- Modify: `src/memory/cortex.rs`

**Steps:**
1. Delete `open_transient_storage` and all call sites (`edges_from`, `edges_to`, `delete_edge`, `update_edge_weight_atomic`, `run_decay`).
2. Replace each with direct calls on `self.storage` (the long-lived `RedbStorage`).
3. Replace O(N) `stats()` traversal with direct `storage.count_edges()` (add helper if missing).
4. Run full test suite — must stay green.
5. Add `tests/no_redb_transient.rs`:

```rust
#[test]
fn no_transient_redb_calls() {
    let s = std::fs::read_to_string("src/memory/cortex.rs").unwrap();
    assert!(!s.contains("open_transient_storage"),
            "transient redb hack must remain removed");
}
```

Commit: `refactor(cortex): remove transient RedbStorage workaround; daemon owns lock`.

---

### Task 1.7: Slice 1 acceptance

**Manual:**
```bash
# Terminal A
vulcan         # start TUI
# Terminal B (TUI still running)
vulcan cortex search "anything"   # must succeed (used to fail with DatabaseAlreadyOpen)
vulcan cortex stats               # under 100ms even with thousands of nodes
```

**Test gate:**
```bash
oo cargo test --features daemon
```

PR: `feat(daemon): YYC-266 Slice 1 — cortex daemon-resident`. Linear YYC-266-B.

---

# SLICE 2 — Full Agent in daemon

**Goal:** Move `Agent` into `SessionState`; light up `prompt.run`, `agent.*`, `approval.*`. TUI ports bottom-up.

**Linear child issues:** YYC-266-C through YYC-266-K (one per sub-PR).

**Detailed plan:** Expand into its own `2026-04-XX-yyc266-slice-2-implementation-plan.md` after Slice 1 lands. The shape below is the PR queue.

### PR queue

| # | Subject | Files | Acceptance |
|---|---|---|---|
| 2.1 | `SessionState::build()` constructs Agent | `daemon/session.rs`, `daemon/resources.rs` | `vulcan __session_build` test command builds agent, no panic |
| 2.2 | `prompt.run` streaming handler | `daemon/handlers/prompt.rs`, `protocol::StreamFrame` writer per-conn | client receives ordered frames text→done |
| 2.3 | `prompt.cancel` | same | mid-stream cancel → `done { cancelled: true }` within 500ms |
| 2.4 | `approval_request` push frame + `approval.respond` | `daemon/handlers/approval.rs`, hook bridge | tool requiring approval pauses; respond unblocks |
| 2.5 | `agent.{status, switch_model, list_models}` | `daemon/handlers/agent.rs` | model swap mid-session reflected in next turn |
| 2.6 | TUI port: stream rendering | `src/tui/mod.rs`, `src/tui/events.rs` | TUI renders streamed text via client; old `Arc<Mutex<Agent>>` path coexists behind feature flag (deleted in 2.10) |
| 2.7 | TUI port: approval overlay | `src/tui/approval.rs` | overlay shows on push frame; user keystroke → `approval.respond` |
| 2.8 | TUI port: cancel (ctrl-c) | `src/tui/events.rs` | ctrl-c sends `prompt.cancel`; UI clears |
| 2.9 | TUI port: model switch + session resume | `src/tui/commands.rs` | both via client |
| 2.10 | Rewire `vulcan prompt`; delete `Agent::builder()` from `main.rs`; greptest forbids `Arc<Mutex<Agent>>` in `tui/` | `src/main.rs`, `tests/no_direct_agent_tui.rs` | `vulcan prompt "hi"` works; greptest green |

**Per-PR TDD:** failing client-side test → handler impl → wire → commit. TUI sub-PRs: spawn daemon under tempdir, drive client, assert on output channel state.

---

# SLICE 3 — Multi-session + Gateway

**Goal:** `session.create/destroy/list` + idle eviction + gateway lanes mapping to sessions.

**Linear child issues:** YYC-266-L, M.

**Detailed plan:** Expand into its own implementation plan after Slice 2 lands.

### PR queue

| # | Subject | Acceptance |
|---|---|---|
| 3.1 | `session.create / destroy / list` | client creates secondary session, isolated from "main" |
| 3.2 | Idle eviction loop (default 30 min, configurable) | low-TTL test config evicts idle session; "main" never evicted |
| 3.3 | `gateway/lane_router.rs` maps `LaneKey → session_id`; lazy `session.create` on first message | two Discord lanes → two sessions |
| 3.4 | `gateway/server.rs` Axum handlers use `Client` | gateway test mode (loopback) drives messages through daemon |
| 3.5 | Delete `src/gateway/agent_map.rs`; greptest forbids reintroduction | gateway tests green |

---

# SLICE 4 — Multi-Agent Collab

**Goal:** RepoIndex, FileReadLog, FileWriteHook, Mailbox, agent-facing tools.

**Linear child issues:** YYC-266-N+ (defined later).

**Detailed plan:** Own brainstorm + implementation plan after Slice 3 lands. Open questions to resolve in that brainstorm:

- Message delivery: at-most-once vs at-least-once + ack
- File-watch granularity: file-level vs chunk/range
- Agent discovery: well-known names, UUIDs, capability tags

### Sketched PR queue (subject to change)

| # | Subject |
|---|---|
| 4.1 | `RepoIndex` populated on `session.create { repo }` |
| 4.2 | `FileReadLog` populated by `AfterToolCall` hook on file-read tools |
| 4.3 | `FileWriteHook` push frame `file_changed` to readers |
| 4.4 | Per-session `Mailbox` + `agent.dm` |
| 4.5 | `agent.broadcast` (all + repo-scoped) |
| 4.6 | Agent-facing tools: `agent_message_send`, `agent_message_recv`, `agent_check_file_changes` |

---

# Cross-cutting tasks (carry forward)

- **Doc:** Add `## Daemon` section to `CLAUDE.md` after Slice 0 lands describing how to interact with the daemon during dev.
- **Memory:** No memory writes from this plan; refactor patterns are derivable from code.
- **Linear hygiene:** Per [split-issues memory](../../../.claude/projects/-home-yycholla-vulcan/memory/feedback_split_issues_per_pr.md), each PR gets its own child Linear issue under YYC-266 epic.

---

# Done definition (whole epic)

- [ ] All 4 slices merged
- [ ] No `Arc<Mutex<Agent>>` in `tui/`, no `open_transient_storage` anywhere
- [ ] `Agent::builder()` callable only from `daemon::session::SessionState::build()`
- [ ] `vulcan` (TUI) and `vulcan cortex search` run concurrently without lock errors
- [ ] `vulcan prompt "hi"` second-call latency under 200ms (was 3-5s cold)
- [ ] Two gateway lanes operate independently in same daemon
- [ ] `~/wiki/queries/rust-hermes-plan.md` updated to reference YYC-266 outcomes
