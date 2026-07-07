# Handoff Report: TUI Compile Failures and Daemon Integration

## 1. Observation

Direct observations from the `rtk cargo check` compiler logs:

- **`src/tui/backend.rs`**:
  - Missing fields in `StreamEvent::ToolCallEnd`:
    ```
    error[E0063]: missing fields `elided_lines` and `output_preview` in initializer of `StreamEvent`
       --> src/tui/backend.rs:278:25
        |
    278 |                         StreamEvent::ToolCallEnd {
        |                         ^^^^^^^^^^^^^^^^^^^^^^^^ missing `elided_lines` and `output_preview`
    ```
  - Tuple vs `StreamFrames` mismatch:
    ```
    error[E0308]: mismatched types
       --> src/tui/backend.rs:253:21
        |
    253 |                   let (mut rx, rx_done) = client.call_stream_at_session...
        |  _____________________^^^^^^^^^^^^^^^^^___-
        | |                     |
        | |                     expected `StreamFrames`, found `(_, _)`
    ```
  - `details` field parsing mismatch (expected `Option<serde_json::Value>`, but string parsing was attempted):
    ```
    details: frame.data.get("details").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
    ```
  - Missing `get_messages` method on `Agent`:
    ```
    error[E0599]: no method named `get_messages` found for struct `tokio::sync::MutexGuard<'_, agent::Agent>` in the current scope
        --> src/tui/backend.rs:147:55
         |
    147  | ...   Self::Direct(agent) => agent.lock().await.get_messages().to_vec(),
         |                                                 ^^^^^^^^^^^^
    ```
  - Missing `cancel` method on `Agent` (should be `cancel_current_turn`):
    ```
    error[E0599]: no method named `cancel` found for struct `tokio::sync::MutexGuard<'_, agent::Agent>` in the current scope
       --> src/tui/backend.rs:173:55
        |
    173 |             Self::Direct(agent) => agent.lock().await.cancel(),
        |                                                       ^^^^^^ method not found in `tokio::sync::MutexGuard<'_, agent::Agent>`
    ```

- **`src/tui/mod.rs`**:
  - `app.diff_sink` double-wrapping mismatch:
    ```
    error[E0308]: mismatched types
       --> src/tui/mod.rs:314:30
        |
    314 |         app.diff_sink = Some(a.diff_sink().await);
        |                              ^^^^^^^^^^^^^^^^^^^ expected `Arc<DiffSink>`, found `Option<Arc<DiffSink>>`
    ```
  - `a.active_profile().map(...)` future mismatch:
    ```
    error[E0599]: `impl Future<Output = Option<String>>` is not an iterator
       --> src/tui/mod.rs:996:70
        |
    995 | / ...                   a.active_profile()
    996 | | ...                       .map(str::to_string),
        | |                           -^^^ `impl Future<Output = Option<String>>` is not an iterator
    ```
  - Missing methods on `TuiBackend`:
    - `available_models` (e.g. at line 1137)
    - `memory` (e.g. at line 349, 1048)
    - `skills` (e.g. at line 929)
    - `orchestration` (e.g. at line 326)
    - `trust_profile` (e.g. at line 321)
    - `restore_persisted_provider` (e.g. at line 283)

- **`src/tui/events.rs`**:
  - Lock error (trying to lock `TuiBackend` directly):
    ```
    error[E0599]: no method named `lock` found for struct `Arc<TuiBackend>` in the current scope
      --> src/tui/events.rs:82:23
       |
    82 |         let mut a = a.lock().await;
       |                       ^^^^ method not found in `Arc<TuiBackend>`
    ```

- **Daemon and `--no-daemon` CLI paths**:
  - In `src/cli.rs`, global clap argument `no_daemon` is defined.
  - In `vulcan/src/main.rs`, the daemon connections are established (`Client::connect_or_autostart()`) and handled. Daemon connection failures are wrapped in `daemon_required_error` to show `vulcan.log` path and recommend `--no-daemon`.

---

## 2. Logic Chain

1. **Cascading closures E0282**: In `backend.rs`, `let (mut rx, rx_done) = client.call_stream_at_session(...)` returned a type mismatch. Since the type of `rx` was unresolved, type inference failed on all downstream frame parsing closures inside the loop (e.g., `and_then(|v| v.as_str())`), producing numerous `E0282` errors. Resolving the destructuring to match `StreamFrames` will fix these cascading errors automatically.
2. **Missing fields in StreamEvent::ToolCallEnd**: The fields `elided_lines` and `output_preview` were added to `StreamEvent::ToolCallEnd` in `src/provider/mod.rs` (to support truncation previews) but the daemon-stream parser in `backend.rs` was never updated to populate them. Adding them resolves the `E0063` error.
3. **`diff_sink` double-wrapping**: `TuiBackend::diff_sink` returns `Option<EditDiffSink>`, but `app.diff_sink` is assigned `Some(a.diff_sink().await)`, which creates `Option<Option<EditDiffSink>>`. Removing the `Some` wrapper fixes this.
4. **`active_profile` map error**: `TuiBackend::active_profile` returns a future resolving to `Option<String>`, not a slice/iterator. Awaiting the future yields `Option<String>`, making the `map(str::to_string)` call redundant and fixing `E0599`.
5. **Direct memory calls on TuiBackend**: The TUI code calls `.memory()` to load history, search messages, and list sessions. Since `TuiBackend` wraps both in-process and daemon connections, exposing raw `SessionStore` locks to TUI violates encapsulation and breaks in the daemon mode. Exposing `list_sessions`, `load_history`, and `search_messages` as direct async methods on `TuiBackend` is a cleaner, unified pattern.
6. **Double `.await` and lock calls**: In `events.rs` line 82, the TUI event loop attempts to call `.lock().await` on `Arc<TuiBackend>`. Since `TuiBackend` is not a Mutex, this fails. Removing the lock is correct as `TuiBackend` methods internally manage concurrency.

---

## 3. Caveats

- The TUI-side `search_messages` and `skills` are stubbed to return empty vectors in the daemon client variant. If real remote search/skills listing via daemon is required in the future, corresponding JSON-RPC daemon endpoints must be implemented.
- The test design assumes that the target platform supports creating files and directories under `VULCAN_HOME` (which is standard).

---

## 4. Conclusion

The compilation failures in `src/tui/backend.rs` and `src/tui/mod.rs` are due to type mismatches and incomplete implementation of the `TuiBackend` enum. Exposing missing methods on `TuiBackend` (`available_models`, `skills`, `orchestration`, `trust_profile`, `restore_persisted_provider`, `load_history`, `search_messages`) and aligning the call sites will resolve the compilation issues.

For daemon errors, wrapping the TUI startup calls in `main.rs` with `daemon_required_error` ensures connection errors show the log path. The regression tests can block the Unix domain socket path using a directory to verify correct connection failure and `--no-daemon` bypass.

---

## 5. Verification Method

1. **Compile Verification**:
   Run `cargo check` (or `rtk cargo check`) and ensure all `src/tui/` compilation errors are resolved.
2. **Regression Test Verification**:
   Execute the newly proposed test cases:
   ```bash
   cargo test --test daemon_escapes_regression
   ```
   Verify that:
   - Blocking `vulcan.sock` causes a standard execution failure printing `vulcan.log` and `--no-daemon`.
   - Running with `--no-daemon` bypasses the daemon connection check completely.
