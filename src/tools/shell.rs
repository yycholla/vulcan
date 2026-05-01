use crate::tools::{Tool, ToolResult, parse_tool_params};
use anyhow::{Context, Result};
use async_trait::async_trait;
use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const DEFAULT_TIMEOUT_SECS: i64 = 60;

fn default_bash_timeout() -> i64 {
    DEFAULT_TIMEOUT_SECS
}

#[derive(Deserialize)]
struct BashParams {
    command: String,
    #[serde(default = "default_bash_timeout")]
    timeout: i64,
    #[serde(default)]
    workdir: Option<String>,
    #[serde(default)]
    use_pty: bool,
    #[serde(default)]
    rows: Option<u64>,
    #[serde(default)]
    cols: Option<u64>,
}

#[derive(Deserialize)]
struct PtyCreateParams {
    #[serde(default)]
    shell: Option<String>,
    #[serde(default)]
    workdir: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    rows: Option<u64>,
    #[serde(default)]
    cols: Option<u64>,
}

#[derive(Deserialize)]
struct PtyWriteParams {
    session_id: String,
    input: String,
}

fn default_pty_max_bytes() -> u64 {
    DEFAULT_READ_BYTES as u64
}

#[derive(Deserialize)]
struct PtyReadParams {
    session_id: String,
    #[serde(default)]
    cursor: u64,
    #[serde(default = "default_pty_max_bytes")]
    max_bytes: u64,
}

#[derive(Deserialize)]
struct PtyResizeParams {
    session_id: String,
    rows: u64,
    cols: u64,
}

#[derive(Deserialize)]
struct PtyCloseParams {
    session_id: String,
}
/// YYC-261: ceiling on the bash tool timeout. Past this, the LLM
/// would effectively be running the command without supervision —
/// long enough for an oversight by the user to lose minutes of
/// progress, short enough that legitimate one-shot scripts (test
/// suites, builds) still fit. One hour matches common CI step caps.
const MAX_TIMEOUT_SECS: i64 = 3600;
/// YYC-261: floor for the bash tool timeout. `0` would race with
/// command startup — the spawn might not have begun before the
/// deadline fires. 1s gives the OS time to actually start the
/// command before the kill arrives.
const MIN_TIMEOUT_SECS: i64 = 1;
const DEFAULT_ROWS: u16 = 24;
const DEFAULT_COLS: u16 = 80;
const DEFAULT_READ_BYTES: usize = 8 * 1024;
const MAX_OUTPUT_CHARS: usize = 50_000;
const SESSION_BUFFER_BYTES: usize = 64 * 1024;
/// Sessions idle for longer than this are closed by the background reaper
/// (YYC-117). 30 minutes matches the issue's default; production agents
/// rarely return to a PTY after that long.
const PTY_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// How often the reaper task scans the session table.
const PTY_REAPER_INTERVAL: Duration = Duration::from_secs(30);

/// YYC-160: per-registry cap on live (non-closed) PTY sessions.
/// One `ToolRegistry` owns one `PtyRegistry`, so this is effectively
/// a per-agent cap. Bursts that try to spawn beyond the cap are
/// refused with an actionable error before we hit `EMFILE`/process
/// limits. Closed sessions don't count — they hold no real
/// resources and are cleaned up by the idle reaper or by the user
/// via `pty_close`.
const MAX_PTY_SESSIONS: usize = 16;

pub fn make_tools() -> Vec<Arc<dyn Tool>> {
    let registry = PtyRegistry::new();
    // YYC-117: spawn an idle reaper so abandoned PTY sessions don't leak
    // their child shells + reader threads. Only fires inside a tokio
    // runtime; synchronous callers (e.g. some unit tests that build a
    // ToolRegistry without a runtime) skip the spawn cleanly.
    registry.clone().spawn_idle_reaper(PTY_IDLE_TIMEOUT);
    vec![
        Arc::new(BashTool),
        Arc::new(PtyCreateTool::new(registry.clone())),
        Arc::new(PtyWriteTool::new(registry.clone())),
        Arc::new(PtyReadTool::new(registry.clone())),
        Arc::new(PtyResizeTool::new(registry.clone())),
        Arc::new(PtyCloseTool::new(registry.clone())),
        Arc::new(PtyListTool::new(registry)),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BufferRead {
    output: String,
    next_cursor: u64,
}

#[derive(Debug)]
struct OutputBuffer {
    start_cursor: u64,
    buf: VecDeque<u8>,
    capacity: usize,
}

impl OutputBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            start_cursor: 0,
            buf: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.buf.push_back(*b);
            while self.buf.len() > self.capacity {
                let _ = self.buf.pop_front();
                self.start_cursor = self.start_cursor.saturating_add(1);
            }
        }
    }

    fn read_from(&self, cursor: u64, max_bytes: usize) -> BufferRead {
        let end_cursor = self.end_cursor();
        let effective_cursor = cursor.max(self.start_cursor).min(end_cursor);
        let offset = (effective_cursor - self.start_cursor) as usize;
        let slice: Vec<u8> = self
            .buf
            .iter()
            .skip(offset)
            .take(max_bytes)
            .copied()
            .collect();
        BufferRead {
            output: String::from_utf8_lossy(&slice).into_owned(),
            next_cursor: effective_cursor + slice.len() as u64,
        }
    }

    fn end_cursor(&self) -> u64 {
        self.start_cursor + self.buf.len() as u64
    }
}

#[derive(Debug, Clone, Serialize)]
struct PtySessionSummary {
    session_id: String,
    name: Option<String>,
    shell: String,
    workdir: Option<String>,
    pid: Option<u32>,
    closed: bool,
}

#[derive(Debug, Clone, Serialize)]
struct PtyReadPayload {
    session_id: String,
    output: String,
    next_cursor: u64,
    closed: bool,
    exit_code: Option<i32>,
}

struct PtySession {
    session_id: String,
    name: Option<String>,
    shell: String,
    workdir: Option<String>,
    pid: Option<u32>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    output: Mutex<OutputBuffer>,
    exit_code: Mutex<Option<i32>>,
    closed: AtomicBool,
    /// Last time the session was read from or written to. The idle reaper
    /// (YYC-117) compares this against `PTY_IDLE_TIMEOUT` to decide which
    /// sessions to close.
    last_used: Mutex<Instant>,
}

impl PtySession {
    fn touch(&self) {
        if let Ok(mut t) = self.last_used.lock() {
            *t = Instant::now();
        }
    }

    fn last_used(&self) -> Instant {
        self.last_used
            .lock()
            .map(|t| *t)
            .unwrap_or_else(|_| Instant::now())
    }
}

impl PtySession {
    fn summary(&self) -> PtySessionSummary {
        PtySessionSummary {
            session_id: self.session_id.clone(),
            name: self.name.clone(),
            shell: self.shell.clone(),
            workdir: self.workdir.clone(),
            pid: self.pid,
            closed: self.closed.load(Ordering::SeqCst),
        }
    }
}

struct PtyRegistry {
    sessions: Mutex<HashMap<String, Arc<PtySession>>>,
    /// YYC-162: idempotent reaper guard. `spawn_idle_reaper` flips
    /// this from false to true atomically; the second caller sees
    /// the prior `true` and bails out so multiple `make_tools()`
    /// calls (or future direct `Arc::clone` paths) can't double-spawn
    /// reapers over the same registry state.
    reaper_started: AtomicBool,
    /// YYC-160: cap on live PTY sessions. Defaults to
    /// `MAX_PTY_SESSIONS`; tests construct a registry with a smaller
    /// cap via `with_cap_for_test` to exercise the cap path
    /// without spawning 17 real shells.
    max_sessions: usize,
}

impl PtyRegistry {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            sessions: Mutex::new(HashMap::new()),
            reaper_started: AtomicBool::new(false),
            max_sessions: MAX_PTY_SESSIONS,
        })
    }

    #[cfg(test)]
    fn with_cap_for_test(cap: usize) -> Arc<Self> {
        Arc::new(Self {
            sessions: Mutex::new(HashMap::new()),
            reaper_started: AtomicBool::new(false),
            max_sessions: cap,
        })
    }

    fn create(
        &self,
        shell: Option<&str>,
        workdir: Option<&str>,
        name: Option<&str>,
        rows: Option<u16>,
        cols: Option<u16>,
    ) -> Result<PtySessionSummary> {
        // YYC-160: enforce per-registry cap before spawning. Counted
        // under the existing sessions lock; closed sessions don't
        // consume real resources and are excluded from the count.
        {
            let sessions = self
                .sessions
                .lock()
                .map_err(|_| anyhow::anyhow!("PTY session registry poisoned"))?;
            let live = sessions
                .values()
                .filter(|s| !s.closed.load(Ordering::SeqCst))
                .count();
            if live >= self.max_sessions {
                anyhow::bail!(
                    "PTY session cap reached ({} live sessions); close an existing session via pty_close before creating a new one",
                    self.max_sessions,
                );
            }
        }
        let shell = shell
            .map(str::to_string)
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "bash".to_string());
        let workdir = workdir.map(str::to_string);
        let pty_system = NativePtySystem::default();
        let pair = pty_system
            .openpty(pty_size(rows, cols))
            .context("Failed to create PTY pair")?;

        let mut cmd = CommandBuilder::new(&shell);
        if shell_supports_interactive_flag(&shell) {
            cmd.arg("-i");
        }
        if let Some(dir) = &workdir {
            cmd.cwd(PathBuf::from(dir));
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("Failed to spawn PTY shell: {shell}"))?;
        let pid = child.process_id();
        let killer = child.clone_killer();
        let reader = pair
            .master
            .try_clone_reader()
            .context("Failed to clone PTY reader")?;
        let writer = pair
            .master
            .take_writer()
            .context("Failed to take PTY writer")?;
        drop(pair.slave);

        let session = Arc::new(PtySession {
            session_id: Uuid::new_v4().to_string(),
            name: name.map(str::to_string),
            shell,
            workdir,
            pid,
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            killer: Mutex::new(killer),
            output: Mutex::new(OutputBuffer::new(SESSION_BUFFER_BYTES)),
            exit_code: Mutex::new(None),
            closed: AtomicBool::new(false),
            last_used: Mutex::new(Instant::now()),
        });

        spawn_reader_thread(reader, session.clone());
        spawn_wait_thread(child, session.clone());

        let summary = session.summary();
        self.sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY session registry poisoned"))?
            .insert(summary.session_id.clone(), session);
        Ok(summary)
    }

    fn list(&self) -> Result<Vec<PtySessionSummary>> {
        Ok(self
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY session registry poisoned"))?
            .values()
            .map(|s| s.summary())
            .collect())
    }

    fn write(&self, session_id: &str, input: &str) -> Result<usize> {
        let session = self.get(session_id)?;
        if session.closed.load(Ordering::SeqCst) {
            anyhow::bail!("PTY session {session_id} is closed");
        }
        let mut writer = session
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY writer poisoned"))?;
        writer
            .write_all(input.as_bytes())
            .with_context(|| format!("Failed to write to PTY session {session_id}"))?;
        writer
            .flush()
            .with_context(|| format!("Failed to flush PTY session {session_id}"))?;
        // YYC-117: count writes as activity for the idle reaper.
        session.touch();
        Ok(input.len())
    }

    fn read(&self, session_id: &str, cursor: u64, max_bytes: usize) -> Result<PtyReadPayload> {
        let session = self.get(session_id)?;
        let read = session
            .output
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY output buffer poisoned"))?
            .read_from(cursor, max_bytes);
        let exit_code = *session
            .exit_code
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY exit status poisoned"))?;
        // YYC-117: count reads as activity for the idle reaper.
        session.touch();
        Ok(PtyReadPayload {
            session_id: session_id.to_string(),
            output: read.output,
            next_cursor: read.next_cursor,
            closed: session.closed.load(Ordering::SeqCst),
            exit_code,
        })
    }

    fn resize(&self, session_id: &str, rows: u16, cols: u16) -> Result<()> {
        let session = self.get(session_id)?;
        if session.closed.load(Ordering::SeqCst) {
            anyhow::bail!("PTY session {session_id} is closed");
        }
        session
            .master
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY master poisoned"))?
            .resize(pty_size(Some(rows), Some(cols)))
            .with_context(|| format!("Failed to resize PTY session {session_id}"))?;
        Ok(())
    }

    fn close(&self, session_id: &str) -> Result<PtySessionSummary> {
        let session = self
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY session registry poisoned"))?
            .remove(session_id)
            .ok_or_else(|| anyhow::anyhow!("Unknown PTY session: {session_id}"))?;

        session.closed.store(true, Ordering::SeqCst);
        if let Ok(mut killer) = session.killer.lock() {
            let _ = killer.kill();
        }
        Ok(session.summary())
    }

    fn close_all(&self) {
        let ids: Vec<String> = match self.sessions.lock() {
            Ok(sessions) => sessions.keys().cloned().collect(),
            Err(_) => return,
        };
        for id in ids {
            let _ = self.close(&id);
        }
    }

    /// Close every session whose `last_used` is older than `idle` (YYC-117).
    /// Returns the IDs that were reaped so callers (tests, telemetry) can
    /// observe what got cleaned up.
    fn close_idle(&self, idle: Duration) -> Vec<String> {
        let now = Instant::now();
        let stale: Vec<String> = match self.sessions.lock() {
            Ok(sessions) => sessions
                .iter()
                .filter_map(|(id, s)| {
                    if now.duration_since(s.last_used()) > idle {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect(),
            Err(_) => return Vec::new(),
        };
        for id in &stale {
            if let Err(e) = self.close(id) {
                tracing::warn!("idle reaper: failed to close PTY {id}: {e}");
            } else {
                tracing::info!("idle reaper: closed PTY session {id}");
            }
        }
        stale
    }

    /// Spawn a background task that periodically reaps idle sessions
    /// (YYC-117). Returns `true` when this call actually spawned the
    /// reaper, `false` when the call was a no-op. No-op cases:
    /// the caller is outside a tokio runtime (synchronous test
    /// harnesses that build a `ToolRegistry` without one) or another
    /// caller already started a reaper for this registry (YYC-162
    /// idempotence).
    fn spawn_idle_reaper(self: Arc<Self>, idle: Duration) -> bool {
        if tokio::runtime::Handle::try_current().is_err() {
            return false;
        }
        // YYC-162: at most one reaper per `PtyRegistry` instance.
        // `swap` is the standard "set-and-test" idiom — the first
        // caller observes `false` and proceeds; later callers see
        // `true` and skip. `Acquire`/`Release` orders the spawn with
        // any subsequent reads of registry state.
        if self.reaper_started.swap(true, Ordering::AcqRel) {
            return false;
        }
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(PTY_REAPER_INTERVAL).await;
                let _ = self.close_idle(idle);
            }
        });
        true
    }

    fn get(&self, session_id: &str) -> Result<Arc<PtySession>> {
        self.sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY session registry poisoned"))?
            .get(session_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Unknown PTY session: {session_id}"))
    }
}

impl Drop for PtyRegistry {
    fn drop(&mut self) {
        self.close_all();
    }
}

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command. Use for builds, installs, git, scripts, and anything else needing a shell. Output is captured and returned. Set use_pty=true for compatibility-sensitive one-shot terminal execution."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute" },
                "timeout": { "type": "integer", "description": "Max seconds to wait", "default": 60, "minimum": 1, "maximum": 300 },
                "workdir": { "type": "string", "description": "Working directory for the command" },
                "use_pty": { "type": "boolean", "description": "Run the one-shot command inside an ephemeral PTY", "default": false },
                "rows": { "type": "integer", "description": "PTY rows when use_pty=true", "default": 24 },
                "cols": { "type": "integer", "description": "PTY columns when use_pty=true", "default": 80 }
            },
            "required": ["command"]
        })
    }

    async fn call(
        &self,
        params: Value,
        cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: BashParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let command = p.command.as_str();
        // YYC-261: clamp the LLM-supplied timeout into the safe range.
        // Negative values would otherwise wrap to ~∞ when cast to u64
        // for `Duration::from_secs`, bypassing the kill-deadline; out-
        // of-range positives would tie up the host. The clamp is
        // silent — `tracing::warn!` if a value lands outside the
        // window so an operator can spot a misconfigured tool call
        // without breaking the run.
        let timeout = clamp_bash_timeout(p.timeout);
        let workdir = p.workdir.as_deref();
        let use_pty = p.use_pty;
        let rows = p.rows.map(|v| v as u16);
        let cols = p.cols.map(|v| v as u16);

        if use_pty {
            return run_one_shot_pty(command, timeout, workdir, cancel, rows, cols).await;
        }

        let mut cmd = tokio::process::Command::new("bash");
        cmd.arg("-c").arg(command);
        cmd.kill_on_drop(true);

        // Always pin cwd explicitly. Without this the spawn inherits
        // whatever ambient cwd the runtime had — which can drift, and
        // some shells/profiles end up resolving to $HOME. workdir param
        // wins; otherwise fall back to the agent's cwd captured up
        // front (process current_dir at the time of the call).
        let resolved_cwd = workdir
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        cmd.current_dir(resolved_cwd);

        let output = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Ok(ToolResult::err("Cancelled"));
            }
            res = tokio::time::timeout(
                Duration::from_secs(timeout as u64),
                cmd.output(),
            ) => res
                .map_err(|_| anyhow::anyhow!("Command timed out after {timeout}s"))??,
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(format_command_result(
            output.status.code().unwrap_or(-1),
            output.status.success(),
            &stdout,
            &stderr,
        ))
    }
}

pub struct PtyCreateTool {
    registry: Arc<PtyRegistry>,
}

impl PtyCreateTool {
    fn new(registry: Arc<PtyRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for PtyCreateTool {
    fn name(&self) -> &str {
        "pty_create"
    }

    fn description(&self) -> &str {
        "Create a persistent interactive PTY shell session. Returns a session_id used by pty_write/pty_read/pty_resize/pty_close."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "shell": { "type": "string", "description": "Shell binary to launch, defaults to $SHELL or bash" },
                "workdir": { "type": "string", "description": "Working directory for the shell session" },
                "name": { "type": "string", "description": "Optional human-readable name for the session" },
                "rows": { "type": "integer", "description": "Initial PTY rows", "default": 24 },
                "cols": { "type": "integer", "description": "Initial PTY columns", "default": 80 }
            }
        })
    }

    async fn call(
        &self,
        params: Value,
        _cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: PtyCreateParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let summary = self.registry.create(
            p.shell.as_deref(),
            p.workdir.as_deref(),
            p.name.as_deref(),
            p.rows.map(|v| v as u16),
            p.cols.map(|v| v as u16),
        )?;
        Ok(ToolResult::ok(serde_json::to_string(&summary)?))
    }
}

pub struct PtyWriteTool {
    registry: Arc<PtyRegistry>,
}

impl PtyWriteTool {
    fn new(registry: Arc<PtyRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for PtyWriteTool {
    fn name(&self) -> &str {
        "pty_write"
    }

    fn description(&self) -> &str {
        "Write raw input to a persistent PTY session."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "description": "Target PTY session id" },
                "input": { "type": "string", "description": "Raw input to send, including newline if needed" }
            },
            "required": ["session_id", "input"]
        })
    }

    async fn call(
        &self,
        params: Value,
        _cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: PtyWriteParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let written = self.registry.write(&p.session_id, &p.input)?;
        Ok(ToolResult::ok(format!(
            "Wrote {written} bytes to PTY session {}",
            p.session_id
        )))
    }
}

pub struct PtyReadTool {
    registry: Arc<PtyRegistry>,
}

impl PtyReadTool {
    fn new(registry: Arc<PtyRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for PtyReadTool {
    fn name(&self) -> &str {
        "pty_read"
    }

    fn description(&self) -> &str {
        "Read incremental output from a persistent PTY session."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "description": "Target PTY session id" },
                "cursor": { "type": "integer", "description": "Read cursor returned by the previous pty_read", "default": 0 },
                "max_bytes": { "type": "integer", "description": "Max bytes to return from the buffer", "default": 8192 }
            },
            "required": ["session_id"]
        })
    }

    async fn call(
        &self,
        params: Value,
        _cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: PtyReadParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let payload = self
            .registry
            .read(&p.session_id, p.cursor, p.max_bytes as usize)?;
        Ok(ToolResult::ok(serde_json::to_string(&payload)?))
    }
}

pub struct PtyResizeTool {
    registry: Arc<PtyRegistry>,
}

impl PtyResizeTool {
    fn new(registry: Arc<PtyRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for PtyResizeTool {
    fn name(&self) -> &str {
        "pty_resize"
    }

    fn description(&self) -> &str {
        "Resize a persistent PTY session."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "description": "Target PTY session id" },
                "rows": { "type": "integer", "description": "New PTY rows" },
                "cols": { "type": "integer", "description": "New PTY columns" }
            },
            "required": ["session_id", "rows", "cols"]
        })
    }

    async fn call(
        &self,
        params: Value,
        _cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: PtyResizeParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let rows = p.rows as u16;
        let cols = p.cols as u16;
        self.registry.resize(&p.session_id, rows, cols)?;
        Ok(ToolResult::ok(format!(
            "Resized PTY session {} to {rows}x{cols}",
            p.session_id
        )))
    }
}

pub struct PtyCloseTool {
    registry: Arc<PtyRegistry>,
}

impl PtyCloseTool {
    fn new(registry: Arc<PtyRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for PtyCloseTool {
    fn name(&self) -> &str {
        "pty_close"
    }

    fn description(&self) -> &str {
        "Close a persistent PTY session."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "description": "Target PTY session id" }
            },
            "required": ["session_id"]
        })
    }

    async fn call(
        &self,
        params: Value,
        _cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: PtyCloseParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let summary = self.registry.close(&p.session_id)?;
        Ok(ToolResult::ok(serde_json::to_string(&summary)?))
    }
}

pub struct PtyListTool {
    registry: Arc<PtyRegistry>,
}

impl PtyListTool {
    fn new(registry: Arc<PtyRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for PtyListTool {
    fn name(&self) -> &str {
        "pty_list"
    }

    fn description(&self) -> &str {
        "List live PTY sessions."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn call(
        &self,
        _params: Value,
        _cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(serde_json::to_string(
            &self.registry.list()?,
        )?))
    }
}

async fn run_one_shot_pty(
    command: &str,
    timeout: i64,
    workdir: Option<&str>,
    cancel: CancellationToken,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<ToolResult> {
    let pty_system = NativePtySystem::default();
    let pair = pty_system
        .openpty(pty_size(rows, cols))
        .context("Failed to create PTY pair")?;

    let mut cmd = CommandBuilder::new("bash");
    // `-c` (not `-lc`) so the shell's login profile doesn't `cd $HOME`
    // out from under the caller's intended workdir.
    cmd.arg("-c");
    cmd.arg(command);
    let resolved_cwd = workdir
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    cmd.cwd(resolved_cwd);

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .context("Failed to spawn PTY bash process")?;
    let mut killer = child.clone_killer();
    let mut reader = pair
        .master
        .try_clone_reader()
        .context("Failed to clone PTY reader")?;
    let writer = pair
        .master
        .take_writer()
        .context("Failed to take PTY writer")?;
    drop(pair.slave);
    drop(writer);

    let output = Arc::new(Mutex::new(OutputBuffer::new(SESSION_BUFFER_BYTES)));
    let reader_output = output.clone();
    std::thread::spawn(move || {
        let mut chunk = [0_u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut buf) = reader_output.lock() {
                        buf.push_bytes(&chunk[..n]);
                    } else {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let (status_tx, status_rx) = oneshot::channel::<i32>();
    std::thread::spawn(move || {
        let code = child
            .wait()
            .ok()
            .map(|s| s.exit_code() as i32)
            .unwrap_or(-1);
        let _ = status_tx.send(code);
    });

    let exit_code = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            let _ = killer.kill();
            return Ok(ToolResult::err("Cancelled"));
        }
        res = tokio::time::timeout(Duration::from_secs(timeout as u64), status_rx) => {
            match res {
                Ok(Ok(code)) => code,
                Ok(Err(_)) => -1,
                Err(_) => {
                    let _ = killer.kill();
                    anyhow::bail!("Command timed out after {timeout}s");
                }
            }
        }
    };

    tokio::time::sleep(Duration::from_millis(25)).await;
    let read = output
        .lock()
        .map_err(|_| anyhow::anyhow!("PTY output buffer poisoned"))?
        .read_from(0, SESSION_BUFFER_BYTES);
    Ok(format_command_result(
        exit_code,
        exit_code == 0,
        &read.output,
        "",
    ))
}

fn spawn_reader_thread(mut reader: Box<dyn Read + Send>, session: Arc<PtySession>) {
    std::thread::spawn(move || {
        let mut chunk = [0_u8; 4096];
        loop {
            // YYC-262: bail before each read if the session has been
            // closed. Normally the child process dying causes `read`
            // to return EOF and the loop exits naturally, but if
            // `PtyRegistry::close` finds itself unable to kill the
            // child (rare — already-exited zombie, EPERM, etc.) the
            // reader would otherwise keep waiting forever holding
            // the master FD. Checking the flag between reads is the
            // cheapest defense against a wedged read.
            if session.closed.load(Ordering::SeqCst) {
                tracing::debug!(
                    session = %session.session_id,
                    "PTY reader exiting because session was marked closed"
                );
                break;
            }
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut output) = session.output.lock() {
                        output.push_bytes(&chunk[..n]);
                    } else {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}

fn spawn_wait_thread(
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    session: Arc<PtySession>,
) {
    std::thread::spawn(move || {
        let code = child.wait().ok().map(|status| status.exit_code() as i32);
        if let Ok(mut exit) = session.exit_code.lock() {
            *exit = code;
        }
        session.closed.store(true, Ordering::SeqCst);
    });
}

/// YYC-261: clamp the LLM-supplied bash timeout into a safe range.
/// Negative values are flagged and replaced with the default; values
/// outside `[MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS]` are clamped at the
/// boundary. Returns the timeout (in seconds) the call should use.
fn clamp_bash_timeout(raw: i64) -> i64 {
    if raw < 0 {
        tracing::warn!("bash timeout {raw} < 0 is invalid; using default {DEFAULT_TIMEOUT_SECS}s");
        return DEFAULT_TIMEOUT_SECS;
    }
    if raw < MIN_TIMEOUT_SECS {
        tracing::warn!("bash timeout {raw} below floor {MIN_TIMEOUT_SECS}; clamping up");
        return MIN_TIMEOUT_SECS;
    }
    if raw > MAX_TIMEOUT_SECS {
        tracing::warn!("bash timeout {raw} above ceiling {MAX_TIMEOUT_SECS}; clamping down");
        return MAX_TIMEOUT_SECS;
    }
    raw
}

fn pty_size(rows: Option<u16>, cols: Option<u16>) -> PtySize {
    PtySize {
        rows: rows.unwrap_or(DEFAULT_ROWS),
        cols: cols.unwrap_or(DEFAULT_COLS),
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn shell_supports_interactive_flag(shell: &str) -> bool {
    let shell_path = PathBuf::from(shell);
    let shell_name = shell_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(shell);
    matches!(shell_name, "bash" | "sh" | "zsh" | "fish")
}

fn format_command_result(exit_code: i32, success: bool, stdout: &str, stderr: &str) -> ToolResult {
    let mut result = String::new();

    if success {
        result.push_str(stdout.trim());
    } else {
        result.push_str(&format!("Exit code: {exit_code}\n"));
        if !stderr.is_empty() {
            result.push_str(&format!("stderr:\n{stderr}"));
        }
        if !stdout.is_empty() {
            if !stderr.is_empty() {
                result.push('\n');
            }
            result.push_str(&format!("output:\n{stdout}"));
        }
    }

    if result.len() > MAX_OUTPUT_CHARS {
        result.truncate(MAX_OUTPUT_CHARS);
        result.push_str("\n... (truncated at 50K chars)");
    }

    ToolResult {
        output: result,
        media: Vec::new(),
        is_error: !success,
        display_preview: None,
        edit_diff: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use tokio::time::sleep;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn yyc263_pty_write_missing_session_id_surfaces_as_toolresult_err() {
        let registry = PtyRegistry::new();
        let tool = PtyWriteTool::new(registry);
        let result = tool
            .call(
                json!({ "input": "echo hi\n" }),
                CancellationToken::new(),
                None,
            )
            .await
            .expect("call returns Ok(ToolResult)");
        assert!(result.is_error);
        assert!(
            result.output.contains("tool params failed to validate"),
            "expected serde-shaped error, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn yyc263_pty_resize_missing_rows_surfaces_as_toolresult_err() {
        let registry = PtyRegistry::new();
        let tool = PtyResizeTool::new(registry);
        let result = tool
            .call(
                json!({ "session_id": "anything", "cols": 80 }),
                CancellationToken::new(),
                None,
            )
            .await
            .expect("call returns Ok(ToolResult)");
        assert!(result.is_error);
        assert!(result.output.contains("tool params failed to validate"));
    }

    #[tokio::test]
    async fn yyc263_bash_missing_command_surfaces_as_toolresult_err() {
        let result = BashTool
            .call(json!({}), CancellationToken::new(), None)
            .await
            .expect("call returns Ok(ToolResult)");
        assert!(result.is_error);
        assert!(
            result.output.contains("tool params failed to validate"),
            "expected serde-shaped error, got: {}",
            result.output
        );
    }

    /// YYC-262: a reader whose underlying source has finite output —
    /// the read function returns N bytes each call, then `Ok(0)` to
    /// signal EOF. Used to drive `spawn_reader_thread` deterministically
    /// inside a test without requiring a real PTY.
    struct FiniteReader {
        chunks: VecDeque<Vec<u8>>,
    }

    impl Read for FiniteReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.chunks.pop_front() {
                Some(chunk) => {
                    let n = chunk.len().min(buf.len());
                    buf[..n].copy_from_slice(&chunk[..n]);
                    Ok(n)
                }
                None => Ok(0),
            }
        }
    }

    fn make_test_session(id: &str) -> Arc<PtySession> {
        // Build a minimal PtySession suitable for the reader thread —
        // we don't need a real master/writer/killer here because the
        // reader thread only touches `session.closed` and `session.output`.
        let pair = NativePtySystem::default()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        Arc::new(PtySession {
            session_id: id.to_string(),
            name: None,
            shell: "test".to_string(),
            workdir: None,
            pid: None,
            master: Mutex::new(pair.master),
            writer: Mutex::new(Box::new(std::io::sink())),
            killer: Mutex::new(Box::new(NoopKiller)),
            output: Mutex::new(OutputBuffer::new(SESSION_BUFFER_BYTES)),
            exit_code: Mutex::new(None),
            closed: AtomicBool::new(false),
            last_used: Mutex::new(Instant::now()),
        })
    }

    #[derive(Debug)]
    struct NoopKiller;
    impl portable_pty::ChildKiller for NoopKiller {
        fn kill(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(NoopKiller)
        }
    }

    #[test]
    fn yyc262_reader_exits_when_session_closed_flag_is_set() {
        let session = make_test_session("test-cancel");
        // The reader will see `closed=true` BEFORE the first read, so
        // the FiniteReader's chunk never lands in the buffer.
        session.closed.store(true, Ordering::SeqCst);
        let reader = Box::new(FiniteReader {
            chunks: VecDeque::from(vec![b"should not be read".to_vec()]),
        });
        spawn_reader_thread(reader, session.clone());
        // Give the spawned thread a moment to observe the flag and exit.
        std::thread::sleep(Duration::from_millis(50));
        // No bytes should have made it into the output buffer.
        let buf = session.output.lock().unwrap();
        assert_eq!(buf.end_cursor(), 0);
    }

    #[test]
    fn yyc262_reader_drains_then_exits_on_eof() {
        let session = make_test_session("test-eof");
        let reader = Box::new(FiniteReader {
            chunks: VecDeque::from(vec![b"hello".to_vec(), b" world".to_vec()]),
        });
        spawn_reader_thread(reader, session.clone());
        std::thread::sleep(Duration::from_millis(50));
        let buf = session.output.lock().unwrap();
        let read = buf.read_from(0, 1024);
        assert_eq!(read.output, "hello world");
    }

    #[test]
    fn yyc261_clamp_bash_timeout_rejects_negative() {
        assert_eq!(clamp_bash_timeout(-1), DEFAULT_TIMEOUT_SECS);
        assert_eq!(clamp_bash_timeout(-9999), DEFAULT_TIMEOUT_SECS);
    }

    #[test]
    fn yyc261_clamp_bash_timeout_clamps_below_floor() {
        assert_eq!(clamp_bash_timeout(0), MIN_TIMEOUT_SECS);
    }

    #[test]
    fn yyc261_clamp_bash_timeout_clamps_above_ceiling() {
        assert_eq!(clamp_bash_timeout(MAX_TIMEOUT_SECS + 1), MAX_TIMEOUT_SECS);
        assert_eq!(clamp_bash_timeout(i64::MAX), MAX_TIMEOUT_SECS);
    }

    #[test]
    fn yyc261_clamp_bash_timeout_passes_through_in_range() {
        for v in [1, 30, 60, 600, MAX_TIMEOUT_SECS] {
            assert_eq!(clamp_bash_timeout(v), v);
        }
    }

    #[test]
    fn output_buffer_reads_incrementally() {
        let mut buf = OutputBuffer::new(1024);
        buf.push_bytes(b"hello");
        let first = buf.read_from(0, 1024);
        assert_eq!(first.output, "hello");
        assert_eq!(first.next_cursor, 5);

        buf.push_bytes(b" world");
        let second = buf.read_from(first.next_cursor, 1024);
        assert_eq!(second.output, " world");
        assert_eq!(second.next_cursor, 11);
    }

    #[test]
    fn output_buffer_discards_old_data_when_capacity_is_exceeded() {
        let mut buf = OutputBuffer::new(5);
        buf.push_bytes(b"hello");
        buf.push_bytes(b" world");

        let read = buf.read_from(0, 1024);
        assert_eq!(read.output, "world");
        assert_eq!(read.next_cursor, 11);
    }

    // YYC-162: spawn_idle_reaper must be idempotent on the same
    // registry. Second call returns false; the AtomicBool stays
    // true. Guards against double-spawned reapers when multiple
    // ToolRegistries (or future Arc::clone callers) share state.
    #[tokio::test]
    async fn spawn_idle_reaper_is_idempotent_within_runtime() {
        let registry = PtyRegistry::new();
        let first = registry.clone().spawn_idle_reaper(Duration::from_secs(60));
        let second = registry.clone().spawn_idle_reaper(Duration::from_secs(60));
        assert!(first, "first call should spawn reaper");
        assert!(!second, "second call must be a no-op");
        assert!(registry.reaper_started.load(Ordering::Acquire));
    }

    // YYC-162: outside a tokio runtime the spawn must return false
    // (not panic) so synchronous callers like tool registry
    // construction in test harnesses can build cleanly.
    #[test]
    fn spawn_idle_reaper_returns_false_without_runtime() {
        let registry = PtyRegistry::new();
        let spawned = registry.clone().spawn_idle_reaper(Duration::from_secs(60));
        assert!(
            !spawned,
            "spawn_idle_reaper must be a no-op without a tokio runtime",
        );
        assert!(
            !registry.reaper_started.load(Ordering::Acquire),
            "reaper_started should remain false when spawn is skipped",
        );
    }

    // YYC-160: the per-registry cap must refuse new sessions once
    // the limit is reached. Uses cap=1 so the test only spawns one
    // real shell, then asserts the second create returns an error
    // mentioning the cap.
    #[cfg(unix)]
    #[tokio::test]
    async fn create_refuses_when_pty_cap_reached() {
        let registry = PtyRegistry::with_cap_for_test(1);
        let _first = registry
            .create(Some("bash"), None, Some("a"), Some(24), Some(80))
            .expect("first create");
        let err = registry
            .create(Some("bash"), None, Some("b"), Some(24), Some(80))
            .expect_err("second create must hit cap");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("cap reached") && msg.contains("pty_close"),
            "unexpected cap error: {msg}",
        );
        registry.close_all();
    }

    // YYC-160: closing a session must free a slot so a follow-up
    // create succeeds. Guards against an off-by-one in the live-count
    // filter or a regression that double-counts closed entries.
    #[cfg(unix)]
    #[tokio::test]
    async fn closing_session_frees_pty_cap_slot() {
        let registry = PtyRegistry::with_cap_for_test(1);
        let first = registry
            .create(Some("bash"), None, Some("a"), Some(24), Some(80))
            .expect("first create");
        // At cap.
        assert!(
            registry
                .create(Some("bash"), None, Some("b"), Some(24), Some(80))
                .is_err()
        );
        registry.close(&first.session_id).expect("close");
        let second = registry
            .create(Some("bash"), None, Some("b"), Some(24), Some(80))
            .expect("second create after close");
        registry.close(&second.session_id).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "flaky pty timing; see YYC-266 prep — needs deterministic ready-signal before unignore"]
    async fn pty_registry_supports_interactive_round_trip() {
        let registry = PtyRegistry::new();
        let summary = registry
            .create(Some("bash"), None, Some("test"), Some(24), Some(80))
            .unwrap();

        registry
            .write(&summary.session_id, "printf 'hi-from-pty\\n'\n")
            .unwrap();

        let mut cursor = 0;
        let mut output = String::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let read = registry
                .read(&summary.session_id, cursor, DEFAULT_READ_BYTES)
                .unwrap();
            cursor = read.next_cursor;
            output.push_str(&read.output);
            if output.contains("hi-from-pty") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for PTY output: {output:?}"
            );
            sleep(Duration::from_millis(25)).await;
        }

        registry.write(&summary.session_id, "exit\n").unwrap();
        let exit_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let read = registry
                .read(&summary.session_id, cursor, DEFAULT_READ_BYTES)
                .unwrap();
            cursor = read.next_cursor;
            output.push_str(&read.output);
            if read.closed {
                break;
            }
            assert!(
                Instant::now() < exit_deadline,
                "timed out waiting for PTY exit: {output:?}"
            );
            sleep(Duration::from_millis(25)).await;
        }

        let summary = registry.close(&summary.session_id).unwrap();
        assert!(summary.closed);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn close_idle_reaps_stale_sessions_only() {
        // YYC-117: a session whose last_used drifted past `idle` should be
        // closed by close_idle; an active session should be left alone.
        let registry = PtyRegistry::new();
        let stale = registry
            .create(Some("bash"), None, Some("stale"), Some(24), Some(80))
            .unwrap();
        let active = registry
            .create(Some("bash"), None, Some("active"), Some(24), Some(80))
            .unwrap();

        let idle = Duration::from_secs(60);

        // Backdate the stale session's last_used so it falls outside the
        // idle window. Active session keeps its just-created last_used.
        {
            let sessions = registry.sessions.lock().unwrap();
            let stale_session = sessions.get(&stale.session_id).unwrap();
            *stale_session.last_used.lock().unwrap() =
                Instant::now() - (idle + Duration::from_secs(1));
        }

        let reaped = registry.close_idle(idle);
        assert_eq!(reaped, vec![stale.session_id.clone()]);

        let remaining = registry.list().unwrap();
        let names: Vec<_> = remaining.iter().filter_map(|s| s.name.clone()).collect();
        assert_eq!(names, vec!["active".to_string()]);

        // Cleanup: close the active session so the test exits cleanly.
        let _ = registry.close(&active.session_id);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bash_tool_supports_one_shot_pty_execution() {
        let result = BashTool
            .call(
                json!({
                    "command": "printf 'hello from bash pty\\n'",
                    "use_pty": true,
                    "timeout": 10
                }),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{result:?}");
        assert!(result.output.contains("hello from bash pty"), "{result:?}");
    }
}
