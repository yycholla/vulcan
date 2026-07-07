# Analysis Report: TUI Compile Failures and Daemon Integration

## Summary of Findings

An investigation was conducted on the compiler failures under `src/tui/` and the integration of the gateway daemon and the `--no-daemon` escape hatch.

We discovered that:

1. The compilation errors are primarily caused by mismatched types resulting from the migration from `Arc<Mutex<Agent>>` to `Arc<TuiBackend>` in TUI state and events, leading to incorrect destructuring and method calls.
2. Direct/Daemon distinction is handled cleanly via the `TuiBackend` abstraction, but several methods are missing or have mismatched interfaces (e.g. `available_models`, `memory` operations, `skills`, `orchestration`, `trust_profile`, `restore_persisted_provider`, and `get_messages`).
3. Daemon connection failures are handled via the `daemon_required_error` helper in `vulcan/src/main.rs`, but TUI subcommands currently lack wrapping for connection errors.
4. The `--no-daemon` escape hatch is defined as a global clap argument in `src/cli.rs` and passed through to one-shot helpers and the TUI runner.

---

## 1. Compiler Errors and Proposed Fixes

### A. `StreamEvent::ToolCallEnd` Initializer in `src/tui/backend.rs`

- **Observation**: In `src/tui/backend.rs` line 278, `StreamEvent::ToolCallEnd` is missing `elided_lines` and `output_preview` fields. Furthermore, `details` is parsed as a `String` whereas `StreamEvent::ToolCallEnd` defines it as `Option<serde_json::Value>`.
- **Fix**: Update the deserialization to map fields correctly:
  ```rust
  StreamEvent::ToolCallEnd {
      id: frame.data.get("tool_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
      name: frame.data.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
      ok: frame.data.get("ok").and_then(|v| v.as_bool()).unwrap_or(true),
      output_preview: frame.data.get("output_preview").and_then(|v| v.as_str()).map(str::to_string),
      details: frame.data.get("details").cloned(),
      result_meta: frame.data.get("result_meta").and_then(|v| serde_json::from_value(v.clone()).ok()),
      elided_lines: frame.data.get("elided_lines").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
      elapsed_ms: frame.data.get("elapsed_ms").and_then(|v| v.as_u64()).unwrap_or_default(),
  }
  ```

### B. Mismatched destructured type for `Client::call_stream_at_session`

- **Observation**: In `src/tui/backend.rs` line 253, `let (mut rx, rx_done) = client.call_stream_at_session(...)` expects a tuple but returns `StreamFrames`.
- **Fix**: Change it to retrieve fields from the returned `StreamFrames` struct:
  ```rust
  let stream_frames = client.call_stream_at_session(&sid, "prompt.stream", serde_json::json!({
      "input": input
  })).await?;
  let mut rx = stream_frames.frames;
  let rx_done = stream_frames.done;
  ```
  _(Note: This type mismatch also caused cascading `E0282` type annotation errors on closures downstream, which will be fully resolved when the correct types are inferred.)_

### C. `app.diff_sink` Type Mismatch in `src/tui/mod.rs`

- **Observation**: In `src/tui/mod.rs` line 314, `app.diff_sink` is assigned `Some(a.diff_sink().await)`. Since `a.diff_sink().await` returns `Option<EditDiffSink>`, wrapping it in `Some` produces `Option<Option<EditDiffSink>>`.
- **Fix**: Remove the redundant `Some` wrap:
  ```rust
  app.diff_sink = a.diff_sink().await;
  ```

### D. `active_profile().map(...)` needing `await` and `FutureExt`

- **Observation**: In `src/tui/mod.rs` line 995, `a.active_profile().map(str::to_string)` fails because `active_profile()` on `TuiBackend` returns `impl Future<Output = Option<String>>`.
- **Fix**: Since the returned option already contains owned `String` objects, simply `await` it:
  ```rust
  a.active_profile().await,
  ```

### E. Missing methods on `&TuiBackend`

To keep the `TuiBackend` abstraction clean and avoid exposing raw `SessionStore` locks to the TUI module, we should implement the needed operations directly on `TuiBackend`:

1. **`available_models`**:
   ```rust
   pub async fn available_models(&self) -> Result<Vec<crate::provider::catalog::ModelInfo>> {
       match self {
           Self::Direct(agent) => agent.lock().await.available_models().await,
           #[cfg(feature = "daemon")]
           Self::Daemon { client, session_id, .. } => {
               let sid = session_id.lock().await.clone();
               let resp = client.call_at_session(&sid, "agent.list_models", serde_json::json!({})).await?;
               if let Some(models_val) = resp.get("models") {
                   if let Some(arr) = models_val.as_array() {
                       let mut models = Vec::new();
                       for v in arr {
                           let id = v.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string();
                           let display_name = v.get("display_name").and_then(|x| x.as_str()).unwrap_or_default().to_string();
                           let context_length = v.get("context_length").and_then(|x| x.as_u64()).unwrap_or_default() as usize;
                           models.push(crate::provider::catalog::ModelInfo {
                               id,
                               display_name,
                               context_length,
                               pricing: None,
                               features: crate::provider::catalog::ModelFeatures::default(),
                               top_provider: None,
                           });
                       }
                       return Ok(models);
                   }
               }
               Ok(vec![])
           }
       }
   }
   ```
2. **`list_sessions`**, **`load_history`**, and **`search_messages`** (replacing `.memory()` calls):

   ```rust
   pub async fn load_history(&self, session_id: &str) -> Result<Option<Vec<Message>>> {
       match self {
           Self::Direct(agent) => agent.lock().await.memory().load_history(session_id).await,
           #[cfg(feature = "daemon")]
           Self::Daemon { client, .. } => {
               let resp = client.call_at_session(session_id, "session.history", serde_json::json!({})).await?;
               if let Some(history) = resp.get("history") {
                   let msgs: Vec<Message> = serde_json::from_value(history.clone())?;
                   Ok(Some(msgs))
               } else {
                   Ok(None)
               }
           }
       }
   }

   pub async fn search_messages(&self, query: &str, limit: usize) -> Result<Vec<crate::memory::SearchHit>> {
       match self {
           Self::Direct(agent) => agent.lock().await.memory().search_messages(query, limit).await,
           #[cfg(feature = "daemon")]
           Self::Daemon { .. } => Ok(vec![]), // Stubbed gracefully
       }
   }
   ```

3. **`skills`**:
   ```rust
   pub async fn skills(&self) -> Vec<crate::skills::Skill> {
       match self {
           Self::Direct(agent) => agent.lock().await.skills().to_vec(),
           #[cfg(feature = "daemon")]
           Self::Daemon { .. } => vec![], // Stubbed gracefully
       }
   }
   ```
4. **`orchestration`** and **`trust_profile`**:

   ```rust
   pub async fn orchestration(&self) -> Arc<crate::orchestration::OrchestrationStore> {
       match self {
           Self::Direct(agent) => agent.lock().await.orchestration().clone(),
           #[cfg(feature = "daemon")]
           Self::Daemon { .. } => Arc::new(crate::orchestration::OrchestrationStore::new()),
       }
   }

   pub async fn trust_profile(&self) -> crate::trust::TrustProfile {
       match self {
           Self::Direct(agent) => agent.lock().await.trust_profile().clone(),
           #[cfg(feature = "daemon")]
           Self::Daemon { .. } => crate::trust::TrustProfile::for_level_with_reason(crate::trust::TrustLevel::Trusted, "Daemon default"),
       }
   }
   ```

5. **`restore_persisted_provider`**:
   ```rust
   pub async fn restore_persisted_provider(&self, config: &Config) -> Result<()> {
       match self {
           Self::Direct(agent) => agent.lock().await.restore_persisted_provider(config).await,
           #[cfg(feature = "daemon")]
           Self::Daemon { .. } => Ok(()), // Managed on daemon side
       }
   }
   ```
6. **`get_messages` on `Agent`** (needed by `TuiBackend` direct path):
   Add the following public getter in `impl Agent` (e.g. in `src/agent/mod.rs`):
   ```rust
   pub fn get_messages(&self) -> &[Message] {
       &self.history_cache
   }
   ```
7. **`cancel()` in TuiBackend**:
   Change `agent.lock().await.cancel()` in `backend.rs` to:
   ```rust
   agent.lock().await.cancel_current_turn()
   ```

### F. Adjusting caller-side usages in TUI Mod / Events

- In `src/tui/events.rs` line 82, remove the `a.lock().await` call entirely since `TuiBackend` manages internal concurrency itself:
  ```rust
  let a = agent.clone();
  tokio::spawn(async move {
      let _ = a.run_prompt_stream_with_cancel(&msg, tx, cancel).await;
  });
  ```
- Rewrite `.memory().list_sessions(...)`, `.memory().load_history(...)`, and `.memory().search_messages(...)` invocations to invoke those methods directly on the `TuiBackend` reference `a`.
- Fix double `.await.await` on `session_id()` in `mod.rs` and `events.rs`.
- Add `.await` for `a.orchestration()` and `a.trust_profile()` in `src/tui/mod.rs`.

---

## 2. CLI Startup and Daemon Connection Mechanics

- **Clap CLI Definition**: In `src/cli.rs`, `Cli` includes a global `no_daemon` argument.
- **TUI Execution Entry**:
  - In `vulcan/src/main.rs` line 136 (`Command::Chat`) and 178 (`Command::Session`), the binary launches `run_tui` passing `cli.no_daemon`.
  - In `run_tui` (defined in `src/tui/mod.rs`), if `no_daemon` is true, the `TuiBackend::Direct(Mutex<Agent>)` is initialized. If `no_daemon` is false, it tries to connect/autostart via `Client::connect_or_autostart().await?`.
- **Daemon Error Wrapping**:
  - Other subcommands (`Prompt`, `Search`, `Cortex`) wrap the daemon connection failure using `daemon_required_error` which displays the log path (`vulcan.log`) and explicitly mentions using `--no-daemon` for direct mode.
  - **Proposed Change**: Wrap TUI connection errors in `main.rs` using `map_err(|e| daemon_required_error("TUI", e))` to ensure that TUI-based connection failures also print the correct log location and the escape hatch instructions.

---

## 3. Regression Test Design

Two end-to-end integration tests should be added to a new test file `tests/daemon_escapes_regression.rs`:

1. **Connection Failure Shows Log Path**:
   - Create a temporary home directory (`VULCAN_HOME`).
   - Create a directory at `<VULCAN_HOME>/vulcan.sock`. Because `vulcan.sock` is a directory instead of a Unix domain socket or a clear path, starting or connecting to the daemon will fail.
   - Run `vulcan prompt hello` (or `vulcan chat`) pointing to this home directory.
   - Assert that the command exits with failure and that `stderr` contains `"vulcan.log"` and the escape hatch option `"--no-daemon"`.

2. **`--no-daemon` Bypasses Connection**:
   - Using the same blocked home directory setup, run `vulcan --no-daemon prompt hello`.
   - Assert that the command does **not** fail with a daemon connection error (e.g. `stderr` does not contain `"daemon prompt failed"`).
