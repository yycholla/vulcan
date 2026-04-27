use crate::tools::{Tool, ToolResult};
use anyhow::{Context, Result};
use async_trait::async_trait;
use portable_pty::{CommandBuilder, MasterPty, NativePtySystem, PtySize, PtySystem};
use serde::Serialize;
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
}

impl PtyRegistry {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            sessions: Mutex::new(HashMap::new()),
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
    /// (YYC-117). No-ops outside a tokio runtime so synchronous callers
    /// (some unit tests that build a `ToolRegistry` without a runtime)
    /// don't panic on `tokio::spawn`.
    fn spawn_idle_reaper(self: Arc<Self>, idle: Duration) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(PTY_REAPER_INTERVAL).await;
                let _ = self.close_idle(idle);
            }
        });
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

    async fn call(&self, params: Value, cancel: CancellationToken) -> Result<ToolResult> {
        let command = params["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("command required"))?;
        let timeout = params["timeout"].as_i64().unwrap_or(DEFAULT_TIMEOUT_SECS);
        let workdir = params["workdir"].as_str();
        let use_pty = params["use_pty"].as_bool().unwrap_or(false);
        let rows = params["rows"].as_u64().map(|v| v as u16);
        let cols = params["cols"].as_u64().map(|v| v as u16);

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

    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let summary = self.registry.create(
            params["shell"].as_str(),
            params["workdir"].as_str(),
            params["name"].as_str(),
            params["rows"].as_u64().map(|v| v as u16),
            params["cols"].as_u64().map(|v| v as u16),
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

    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let session_id = params["session_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("session_id required"))?;
        let input = params["input"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("input required"))?;
        let written = self.registry.write(session_id, input)?;
        Ok(ToolResult::ok(format!(
            "Wrote {written} bytes to PTY session {session_id}"
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

    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let session_id = params["session_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("session_id required"))?;
        let payload = self.registry.read(
            session_id,
            params["cursor"].as_u64().unwrap_or(0),
            params["max_bytes"]
                .as_u64()
                .unwrap_or(DEFAULT_READ_BYTES as u64) as usize,
        )?;
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

    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let session_id = params["session_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("session_id required"))?;
        let rows = params["rows"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("rows required"))? as u16;
        let cols = params["cols"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("cols required"))? as u16;
        self.registry.resize(session_id, rows, cols)?;
        Ok(ToolResult::ok(format!(
            "Resized PTY session {session_id} to {rows}x{cols}"
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

    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let session_id = params["session_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("session_id required"))?;
        let summary = self.registry.close(session_id)?;
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

    async fn call(&self, _params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
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

    #[cfg(unix)]
    #[tokio::test]
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
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{result:?}");
        assert!(result.output.contains("hello from bash pty"), "{result:?}");
    }
}
