//! YYC-179: durable execution timeline for agent runs.
//!
//! Every CLI/TUI/gateway turn gets a stable `RunId` and accumulates
//! a typed event stream — provider calls, tool dispatches, hook
//! decisions, lifecycle transitions. Events are persisted to a
//! SQLite store under `~/.vulcan/run_records.db` so post-hoc
//! debugging, replay, and the YYC-181 PR-4 profile-visibility ride
//! the same surface.
//!
//! ## Scope of this PR
//!
//! - `RunId` newtype + `RunStatus` enum.
//! - `RunEvent` family covering lifecycle / provider / tool / hook /
//!   subagent / artifact (artifact is a placeholder shape pending
//!   YYC-180).
//! - `Redacted` helper: store fingerprints (`sha256:<hex>`) instead
//!   of raw payloads by default.
//! - `RunStore` with both an in-memory and a SQLite backend.
//! - `RunRecorder` interface so the agent loop and tool dispatch
//!   write events through one tiny abstraction (set in PR-2/3).
//!
//! ## Deliberately deferred
//!
//! - Wiring into the agent loop / dispatch / hooks / provider
//!   (PR-2..PR-5).
//! - `vulcan run show <id>` CLI surface (PR-6).
//! - Gateway lane metadata (waits on PR-2).
//! - Replay/reproduce — that's a sibling issue (YYC-184).

use anyhow::{Context, Result};
use async_trait::async_trait;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

/// Stable identifier for a single agent turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RunId(Uuid);

impl RunId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Lifecycle state of a run. Mirrors the orchestration child-status
/// shape so the two surfaces stay aligned (subagent runs are also
/// runs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Created,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
        )
    }
}

/// Where a run originated. Lets the query surface filter "show me
/// every gateway turn last hour" without scanning the world.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOrigin {
    Cli,
    Tui,
    Gateway { lane: String },
    Subagent { parent_run_id: RunId },
    Other(String),
}

/// SHA-256 fingerprint of a payload — used in place of the raw
/// string when the field is sensitive (prompts, tool args, model
/// outputs) and the operator hasn't opted into raw retention.
///
/// Format: `sha256:<lowercase-hex>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PayloadFingerprint(String);

impl PayloadFingerprint {
    /// Compute the fingerprint of `payload`. Same input always
    /// produces the same fingerprint, so equal-payload comparisons
    /// in run records still work after redaction.
    pub fn of(payload: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(payload);
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(7 + digest.len() * 2);
        hex.push_str("sha256:");
        for byte in digest.iter() {
            hex.push_str(&format!("{:02x}", byte));
        }
        Self(hex)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One piece of evidence about how a run unfolded. Variants stay
/// flat (no nested boxes) so the SQLite codec can serialize them as
/// JSON without recursion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunEvent {
    /// Lifecycle: status transition.
    StatusChanged {
        status: RunStatus,
    },
    /// User input arrived. Default to fingerprint; raw text lands
    /// only if `keep_raw` is true.
    PromptReceived {
        fingerprint: PayloadFingerprint,
        char_count: usize,
        raw: Option<String>,
    },
    /// Provider call started. Captures model + mode but not the
    /// joined message body (fingerprint that separately when
    /// useful).
    ProviderRequest {
        model: String,
        streaming: bool,
        message_count: usize,
    },
    /// Provider call finished — usage + finish reason. Errors come
    /// through `ProviderError` instead.
    ProviderResponse {
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        finish_reason: Option<String>,
    },
    ProviderError {
        message: String,
        retryable: bool,
    },
    /// Hook fired. `outcome` is a short tag like "continue" /
    /// "block" / "replace_args" / "inject" so dashboards aggregate
    /// without parsing.
    HookDecision {
        event: String,
        handler: String,
        outcome: String,
        detail: Option<String>,
    },
    ToolCall {
        name: String,
        args_fingerprint: PayloadFingerprint,
        approval: Option<String>,
        duration_ms: u64,
        is_error: bool,
        error: Option<String>,
    },
    SubagentSpawned {
        child_run_id: RunId,
        task_summary: String,
    },
    /// Placeholder for YYC-180. The id is a free-form string today;
    /// once the artifact system lands, it'll be an `ArtifactId`.
    ArtifactCreated {
        artifact_id: String,
        artifact_type: String,
    },
    /// YYC-182: workspace trust profile resolved at run start.
    /// `level` is the canonical lowercase tag (`trusted`,
    /// `restricted`, `sensitive`, `untrusted`). `reason` is the
    /// resolver's free-form explanation surfaced by `vulcan trust
    /// why` and `vulcan run show`.
    TrustResolved {
        level: String,
        capability_profile: String,
        reason: String,
        allow_indexing: bool,
        allow_persistence: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: RunId,
    pub origin: RunOrigin,
    pub session_id: Option<String>,
    pub workspace: Option<String>,
    pub model: Option<String>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    pub status: RunStatus,
    pub events: Vec<RunEvent>,
    pub error: Option<String>,
}

impl RunRecord {
    pub fn new(origin: RunOrigin) -> Self {
        Self {
            id: RunId::new(),
            origin,
            session_id: None,
            workspace: None,
            model: None,
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Created,
            events: Vec::new(),
            error: None,
        }
    }
}

/// Storage backend for run records. Both backends append events,
/// finalize on terminal status, and query recent records by id /
/// time.
#[async_trait]
pub trait RunStore: Send + Sync {
    async fn create(&self, record: &RunRecord) -> Result<()>;
    async fn append_event(&self, run_id: RunId, event: RunEvent) -> Result<()>;
    async fn finalize(&self, run_id: RunId, status: RunStatus, error: Option<String>)
    -> Result<()>;
    async fn get(&self, run_id: RunId) -> Result<Option<RunRecord>>;
    async fn recent(&self, limit: usize) -> Result<Vec<RunRecord>>;
}

/// In-memory backend — fine for tests + the no-DB code paths.
/// Default cap of 256 keeps memory bounded; ring drops oldest first.
#[derive(Debug)]
pub struct InMemoryRunStore {
    inner: Mutex<Vec<RunRecord>>,
    cap: usize,
}

impl Default for InMemoryRunStore {
    fn default() -> Self {
        Self::new(256)
    }
}

impl InMemoryRunStore {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
            cap: cap.max(1),
        }
    }
}

#[async_trait]
impl RunStore for InMemoryRunStore {
    async fn create(&self, record: &RunRecord) -> Result<()> {
        let mut guard = self.inner.lock();
        if guard.len() >= self.cap {
            guard.remove(0);
        }
        guard.push(record.clone());
        Ok(())
    }

    async fn append_event(&self, run_id: RunId, event: RunEvent) -> Result<()> {
        let mut guard = self.inner.lock();
        if let Some(rec) = guard.iter_mut().find(|r| r.id == run_id) {
            if let RunEvent::StatusChanged { status } = &event {
                rec.status = *status;
            }
            rec.events.push(event);
        }
        Ok(())
    }

    async fn finalize(
        &self,
        run_id: RunId,
        status: RunStatus,
        error: Option<String>,
    ) -> Result<()> {
        let mut guard = self.inner.lock();
        if let Some(rec) = guard.iter_mut().find(|r| r.id == run_id) {
            rec.status = status;
            rec.ended_at = Some(chrono::Utc::now());
            rec.error = error;
            rec.events.push(RunEvent::StatusChanged { status });
        }
        Ok(())
    }

    async fn get(&self, run_id: RunId) -> Result<Option<RunRecord>> {
        Ok(self.inner.lock().iter().find(|r| r.id == run_id).cloned())
    }

    async fn recent(&self, limit: usize) -> Result<Vec<RunRecord>> {
        let guard = self.inner.lock();
        Ok(guard.iter().rev().take(limit).cloned().collect())
    }
}

/// SQLite backend. Two tables: `runs` (one row per run, mutable
/// status + ended_at + error) and `run_events` (append-only,
/// ordered by autoincrement id). Event payloads ride as JSON so
/// schema changes to `RunEvent` don't require a migration unless
/// they remove a variant.
pub struct SqliteRunStore {
    conn: Mutex<Connection>,
}

impl SqliteRunStore {
    pub fn try_new() -> Result<Self> {
        let dir = crate::config::vulcan_home();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create vulcan_home at {}", dir.display()))?;
        Self::try_open_at(&dir.join("run_records.db"))
    }

    pub fn try_open_at(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("open run records DB at {}", path.display()))?;
        Self::initialize(&conn)
            .with_context(|| format!("init run records schema at {}", path.display()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn try_open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory run records DB")?;
        Self::initialize(&conn).context("init in-memory run records schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn initialize(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS runs (
                id           TEXT PRIMARY KEY,
                origin       TEXT NOT NULL,
                session_id   TEXT,
                workspace    TEXT,
                model        TEXT,
                started_at   TEXT NOT NULL,
                ended_at     TEXT,
                status       TEXT NOT NULL,
                error        TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_runs_started_at ON runs(started_at DESC);

            CREATE TABLE IF NOT EXISTS run_events (
                seq      INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id   TEXT NOT NULL,
                payload  TEXT NOT NULL,
                FOREIGN KEY (run_id) REFERENCES runs(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_run_events_run_id ON run_events(run_id, seq);
            "#,
        )?;
        Ok(())
    }
}

#[async_trait]
impl RunStore for SqliteRunStore {
    async fn create(&self, record: &RunRecord) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO runs (id, origin, session_id, workspace, model, started_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.id.to_string(),
                serde_json::to_string(&record.origin)?,
                record.session_id,
                record.workspace,
                record.model,
                record.started_at.to_rfc3339(),
                serde_json::to_string(&record.status)?,
            ],
        )?;
        Ok(())
    }

    async fn append_event(&self, run_id: RunId, event: RunEvent) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO run_events (run_id, payload) VALUES (?1, ?2)",
            params![run_id.to_string(), serde_json::to_string(&event)?],
        )?;
        if let RunEvent::StatusChanged { status } = &event {
            conn.execute(
                "UPDATE runs SET status = ?1 WHERE id = ?2",
                params![serde_json::to_string(status)?, run_id.to_string()],
            )?;
        }
        Ok(())
    }

    async fn finalize(
        &self,
        run_id: RunId,
        status: RunStatus,
        error: Option<String>,
    ) -> Result<()> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE runs SET status = ?1, ended_at = ?2, error = ?3 WHERE id = ?4",
            params![
                serde_json::to_string(&status)?,
                now,
                error,
                run_id.to_string()
            ],
        )?;
        conn.execute(
            "INSERT INTO run_events (run_id, payload) VALUES (?1, ?2)",
            params![
                run_id.to_string(),
                serde_json::to_string(&RunEvent::StatusChanged { status })?
            ],
        )?;
        Ok(())
    }

    async fn get(&self, run_id: RunId) -> Result<Option<RunRecord>> {
        let conn = self.conn.lock();
        let id_str = run_id.to_string();
        let row = conn
            .query_row(
                "SELECT origin, session_id, workspace, model, started_at, ended_at, status, error
                 FROM runs WHERE id = ?1",
                params![id_str],
                |row| {
                    let origin: String = row.get(0)?;
                    let session_id: Option<String> = row.get(1)?;
                    let workspace: Option<String> = row.get(2)?;
                    let model: Option<String> = row.get(3)?;
                    let started_at: String = row.get(4)?;
                    let ended_at: Option<String> = row.get(5)?;
                    let status: String = row.get(6)?;
                    let error: Option<String> = row.get(7)?;
                    Ok((
                        origin, session_id, workspace, model, started_at, ended_at, status, error,
                    ))
                },
            )
            .optional()?;
        let Some((origin, session_id, workspace, model, started_at, ended_at, status, error)) = row
        else {
            return Ok(None);
        };
        let mut stmt =
            conn.prepare("SELECT payload FROM run_events WHERE run_id = ?1 ORDER BY seq ASC")?;
        let events = stmt
            .query_map(params![id_str], |row| row.get::<_, String>(0))?
            .map(|res| {
                res.map_err(anyhow::Error::from)
                    .and_then(|raw| serde_json::from_str::<RunEvent>(&raw).map_err(Into::into))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(RunRecord {
            id: run_id,
            origin: serde_json::from_str(&origin)?,
            session_id,
            workspace,
            model,
            started_at: chrono::DateTime::parse_from_rfc3339(&started_at)?
                .with_timezone(&chrono::Utc),
            ended_at: ended_at
                .map(|s| {
                    chrono::DateTime::parse_from_rfc3339(&s).map(|d| d.with_timezone(&chrono::Utc))
                })
                .transpose()?,
            status: serde_json::from_str(&status)?,
            events,
            error,
        }))
    }

    async fn recent(&self, limit: usize) -> Result<Vec<RunRecord>> {
        // Scope the connection + statement so both are dropped before
        // the `self.get(id).await` loop below — a `Statement` is !Send,
        // and the async Send check keeps the binding's slot alive across
        // an await even after an explicit `drop`, so a block is needed.
        let ids: Vec<String> = {
            let conn = self.conn.lock();
            let mut stmt = conn.prepare("SELECT id FROM runs ORDER BY started_at DESC LIMIT ?1")?;
            stmt.query_map(params![limit as i64], |row| row.get::<_, String>(0))?
                .collect::<Result<_, _>>()?
        };
        let mut out = Vec::with_capacity(ids.len());
        for raw_id in ids {
            let id = RunId::from_uuid(Uuid::parse_str(&raw_id)?);
            if let Some(rec) = self.get(id).await? {
                out.push(rec);
            }
        }
        Ok(out)
    }
}

/// Open the default run store. Selects the backend at compile time:
/// Turso under `turso-backend` (GH #704), else rusqlite.
pub async fn open_default_store() -> Result<Arc<dyn RunStore>> {
    #[cfg(feature = "turso-backend")]
    {
        Ok(Arc::new(TursoRunStore::try_new().await?))
    }
    #[cfg(not(feature = "turso-backend"))]
    {
        Ok(Arc::new(SqliteRunStore::try_new()?))
    }
}

/// Turso-backed run store (GH #704). Async; bare `turso::Connection`,
/// no `Mutex`/r2d2/`spawn_blocking`. Same two-table shape (runs +
/// append-only run_events with JSON payloads) as rusqlite.
#[cfg(feature = "turso-backend")]
pub struct TursoRunStore {
    conn: turso::Connection,
}

#[cfg(feature = "turso-backend")]
impl TursoRunStore {
    pub async fn try_new() -> Result<Self> {
        let path = crate::config::vulcan_home().join("run_records.db");
        Self::try_open_at(&path).await
    }

    pub async fn try_open_at(path: &Path) -> Result<Self> {
        let conn = crate::db::open(path).await?;
        Self::initialize(&conn).await?;
        Ok(Self { conn })
    }

    pub async fn try_open_in_memory() -> Result<Self> {
        let conn = crate::db::open_in_memory().await?;
        Self::initialize(&conn).await?;
        Ok(Self { conn })
    }

    async fn initialize(conn: &turso::Connection) -> Result<()> {
        conn.execute(
            "CREATE TABLE IF NOT EXISTS runs (
                id TEXT PRIMARY KEY, origin TEXT NOT NULL, session_id TEXT, workspace TEXT,
                model TEXT, started_at TEXT NOT NULL, ended_at TEXT, status TEXT NOT NULL, error TEXT
            )",
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_runs_started_at ON runs(started_at DESC)",
            (),
        )
        .await?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS run_events (
                seq INTEGER PRIMARY KEY AUTOINCREMENT, run_id TEXT NOT NULL, payload TEXT NOT NULL
            )",
            (),
        )
        .await?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_run_events_run_id ON run_events(run_id, seq)",
            (),
        )
        .await?;
        Ok(())
    }
}

#[cfg(feature = "turso-backend")]
#[async_trait]
impl RunStore for TursoRunStore {
    async fn create(&self, record: &RunRecord) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO runs (id, origin, session_id, workspace, model, started_at, status) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                turso::params_from_iter([
                    turso::Value::from(record.id.to_string()),
                    turso::Value::from(serde_json::to_string(&record.origin)?),
                    record.session_id.clone().into(),
                    record.workspace.clone().into(),
                    record.model.clone().into(),
                    turso::Value::from(record.started_at.to_rfc3339()),
                    turso::Value::from(serde_json::to_string(&record.status)?),
                ]),
            )
            .await?;
        Ok(())
    }

    async fn append_event(&self, run_id: RunId, event: RunEvent) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO run_events (run_id, payload) VALUES (?1, ?2)",
                (run_id.to_string(), serde_json::to_string(&event)?),
            )
            .await?;
        if let RunEvent::StatusChanged { status } = &event {
            self.conn
                .execute(
                    "UPDATE runs SET status = ?1 WHERE id = ?2",
                    (serde_json::to_string(status)?, run_id.to_string()),
                )
                .await?;
        }
        Ok(())
    }

    async fn finalize(
        &self,
        run_id: RunId,
        status: RunStatus,
        error: Option<String>,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn
            .execute(
                "UPDATE runs SET status = ?1, ended_at = ?2, error = ?3 WHERE id = ?4",
                turso::params_from_iter([
                    turso::Value::from(serde_json::to_string(&status)?),
                    turso::Value::from(now),
                    error.into(),
                    turso::Value::from(run_id.to_string()),
                ]),
            )
            .await?;
        self.conn
            .execute(
                "INSERT INTO run_events (run_id, payload) VALUES (?1, ?2)",
                (
                    run_id.to_string(),
                    serde_json::to_string(&RunEvent::StatusChanged { status })?,
                ),
            )
            .await?;
        Ok(())
    }

    async fn get(&self, run_id: RunId) -> Result<Option<RunRecord>> {
        let id_str = run_id.to_string();
        let mut rows = self
            .conn
            .query(
                "SELECT origin, session_id, workspace, model, started_at, ended_at, status, error \
                 FROM runs WHERE id = ?1",
                (id_str.clone(),),
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let origin: String = row.get(0)?;
        let session_id: Option<String> = row.get(1)?;
        let workspace: Option<String> = row.get(2)?;
        let model: Option<String> = row.get(3)?;
        let started_at: String = row.get(4)?;
        let ended_at: Option<String> = row.get(5)?;
        let status: String = row.get(6)?;
        let error: Option<String> = row.get(7)?;

        let mut ev_rows = self
            .conn
            .query(
                "SELECT payload FROM run_events WHERE run_id = ?1 ORDER BY seq ASC",
                (id_str,),
            )
            .await?;
        let mut events = Vec::new();
        while let Some(ev) = ev_rows.next().await? {
            let raw: String = ev.get(0)?;
            events.push(serde_json::from_str::<RunEvent>(&raw)?);
        }

        Ok(Some(RunRecord {
            id: run_id,
            origin: serde_json::from_str(&origin)?,
            session_id,
            workspace,
            model,
            started_at: chrono::DateTime::parse_from_rfc3339(&started_at)?
                .with_timezone(&chrono::Utc),
            ended_at: ended_at
                .map(|s| {
                    chrono::DateTime::parse_from_rfc3339(&s).map(|d| d.with_timezone(&chrono::Utc))
                })
                .transpose()?,
            status: serde_json::from_str(&status)?,
            events,
            error,
        }))
    }

    async fn recent(&self, limit: usize) -> Result<Vec<RunRecord>> {
        let mut rows = self
            .conn
            .query(
                "SELECT id FROM runs ORDER BY started_at DESC LIMIT ?1",
                (limit as i64,),
            )
            .await?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await? {
            let raw: String = row.get(0)?;
            ids.push(raw);
        }
        let mut out = Vec::with_capacity(ids.len());
        for raw_id in ids {
            let id = RunId::from_uuid(Uuid::parse_str(&raw_id)?);
            if let Some(rec) = self.get(id).await? {
                out.push(rec);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event() -> RunEvent {
        RunEvent::ToolCall {
            name: "read_file".into(),
            args_fingerprint: PayloadFingerprint::of(b"{\"path\":\"x\"}"),
            approval: None,
            duration_ms: 12,
            is_error: false,
            error: None,
        }
    }

    #[test]
    fn payload_fingerprint_is_stable_and_redacts() {
        let a = PayloadFingerprint::of(b"hello world");
        let b = PayloadFingerprint::of(b"hello world");
        let c = PayloadFingerprint::of(b"different");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.as_str().starts_with("sha256:"));
        // Fingerprint must not contain raw input.
        assert!(!a.as_str().contains("hello"));
    }

    #[test]
    fn run_status_terminal_classification() {
        assert!(!RunStatus::Created.is_terminal());
        assert!(!RunStatus::Running.is_terminal());
        assert!(RunStatus::Completed.is_terminal());
        assert!(RunStatus::Failed.is_terminal());
        assert!(RunStatus::Cancelled.is_terminal());
    }

    #[tokio::test]
    async fn in_memory_store_records_full_lifecycle() {
        let store = InMemoryRunStore::default();
        let mut record = RunRecord::new(RunOrigin::Cli);
        record.session_id = Some("sess-1".into());
        let id = record.id;
        store.create(&record).await.unwrap();
        store
            .append_event(
                id,
                RunEvent::StatusChanged {
                    status: RunStatus::Running,
                },
            )
            .await
            .unwrap();
        store.append_event(id, sample_event()).await.unwrap();
        store
            .finalize(id, RunStatus::Completed, None)
            .await
            .unwrap();
        let got = store.get(id).await.unwrap().expect("stored");
        assert_eq!(got.status, RunStatus::Completed);
        assert!(got.ended_at.is_some());
        // Three events: Running, ToolCall, Completed.
        assert_eq!(got.events.len(), 3);
    }

    #[tokio::test]
    async fn in_memory_store_caps_records() {
        let store = InMemoryRunStore::new(2);
        let r1 = RunRecord::new(RunOrigin::Cli);
        let r2 = RunRecord::new(RunOrigin::Tui);
        let r3 = RunRecord::new(RunOrigin::Other("test".into()));
        store.create(&r1).await.unwrap();
        store.create(&r2).await.unwrap();
        store.create(&r3).await.unwrap();
        // r1 evicted under FIFO drop.
        assert!(store.get(r1.id).await.unwrap().is_none());
        assert!(store.get(r2.id).await.unwrap().is_some());
        assert!(store.get(r3.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn sqlite_store_round_trip() {
        let store = SqliteRunStore::try_open_in_memory().expect("open in-memory");
        let mut record = RunRecord::new(RunOrigin::Tui);
        record.model = Some("opus-4".into());
        let id = record.id;
        store.create(&record).await.unwrap();

        store
            .append_event(
                id,
                RunEvent::PromptReceived {
                    fingerprint: PayloadFingerprint::of(b"do the thing"),
                    char_count: 12,
                    raw: None,
                },
            )
            .await
            .unwrap();
        store
            .append_event(
                id,
                RunEvent::ProviderRequest {
                    model: "opus-4".into(),
                    streaming: true,
                    message_count: 3,
                },
            )
            .await
            .unwrap();
        store.append_event(id, sample_event()).await.unwrap();
        store
            .finalize(id, RunStatus::Completed, None)
            .await
            .unwrap();

        let got = store.get(id).await.unwrap().expect("present");
        assert_eq!(got.status, RunStatus::Completed);
        assert_eq!(got.model.as_deref(), Some("opus-4"));
        assert!(got.ended_at.is_some());
        assert_eq!(got.events.len(), 4);
        // Event 0 should be PromptReceived with no raw and no leak.
        match &got.events[0] {
            RunEvent::PromptReceived {
                fingerprint,
                raw,
                char_count,
            } => {
                assert!(raw.is_none());
                assert_eq!(*char_count, 12);
                assert!(fingerprint.as_str().starts_with("sha256:"));
            }
            other => panic!("unexpected first event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn sqlite_store_failure_is_distinguishable_from_success() {
        let store = SqliteRunStore::try_open_in_memory().unwrap();
        let record = RunRecord::new(RunOrigin::Cli);
        let id = record.id;
        store.create(&record).await.unwrap();
        store
            .append_event(
                id,
                RunEvent::ProviderError {
                    message: "503 upstream".into(),
                    retryable: true,
                },
            )
            .await
            .unwrap();
        store
            .finalize(id, RunStatus::Failed, Some("provider down".into()))
            .await
            .unwrap();
        let got = store.get(id).await.unwrap().expect("present");
        assert_eq!(got.status, RunStatus::Failed);
        assert_eq!(got.error.as_deref(), Some("provider down"));
        assert!(matches!(got.events[0], RunEvent::ProviderError { .. }));
    }

    #[tokio::test]
    async fn sqlite_store_blocked_hook_is_visible() {
        // Acceptance: blocked hook decisions and tool errors must be
        // distinguishable from successful tool calls. We assert the
        // event variant directly because the schema is designed so
        // dashboards can group by `outcome`.
        let store = SqliteRunStore::try_open_in_memory().unwrap();
        let record = RunRecord::new(RunOrigin::Cli);
        let id = record.id;
        store.create(&record).await.unwrap();
        store
            .append_event(
                id,
                RunEvent::HookDecision {
                    event: "before_tool_call".into(),
                    handler: "safety".into(),
                    outcome: "block".into(),
                    detail: Some("rm -rf /".into()),
                },
            )
            .await
            .unwrap();
        store
            .append_event(
                id,
                RunEvent::ToolCall {
                    name: "bash".into(),
                    args_fingerprint: PayloadFingerprint::of(b"rm -rf /"),
                    approval: Some("denied".into()),
                    duration_ms: 0,
                    is_error: true,
                    error: Some("blocked by safety hook".into()),
                },
            )
            .await
            .unwrap();
        store
            .finalize(id, RunStatus::Completed, None)
            .await
            .unwrap();
        let got = store.get(id).await.unwrap().unwrap();
        let outcomes: Vec<&str> = got
            .events
            .iter()
            .filter_map(|e| match e {
                RunEvent::HookDecision { outcome, .. } => Some(outcome.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(outcomes, vec!["block"]);
        let tool_errs: Vec<bool> = got
            .events
            .iter()
            .filter_map(|e| match e {
                RunEvent::ToolCall { is_error, .. } => Some(*is_error),
                _ => None,
            })
            .collect();
        assert_eq!(tool_errs, vec![true]);
    }

    #[tokio::test]
    async fn sqlite_store_recent_returns_newest_first() {
        let store = SqliteRunStore::try_open_in_memory().unwrap();
        let r1 = RunRecord::new(RunOrigin::Cli);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let r2 = RunRecord::new(RunOrigin::Tui);
        store.create(&r1).await.unwrap();
        store.create(&r2).await.unwrap();
        let recent = store.recent(10).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, r2.id);
        assert_eq!(recent[1].id, r1.id);
    }
}

// GH #704: the Turso backend satisfies the same RunStore contract.
#[cfg(all(test, feature = "turso-backend"))]
mod turso_tests {
    use super::*;

    #[tokio::test]
    async fn turso_store_round_trips_lifecycle_and_timeline() {
        let store = TursoRunStore::try_open_in_memory().await.unwrap();
        let mut rec = RunRecord::new(RunOrigin::Cli);
        rec.session_id = Some("sess-1".into());
        rec.model = Some("test-model".into());
        let id = rec.id;
        store.create(&rec).await.unwrap();

        store
            .append_event(
                id,
                RunEvent::StatusChanged {
                    status: RunStatus::Running,
                },
            )
            .await
            .unwrap();
        store
            .append_event(
                id,
                RunEvent::PromptReceived {
                    fingerprint: PayloadFingerprint::of(b"hello"),
                    char_count: 5,
                    raw: None,
                },
            )
            .await
            .unwrap();
        store
            .finalize(id, RunStatus::Completed, None)
            .await
            .unwrap();

        let got = store.get(id).await.unwrap().expect("run present");
        assert_eq!(got.status, RunStatus::Completed);
        assert_eq!(got.session_id.as_deref(), Some("sess-1"));
        assert!(got.ended_at.is_some());
        assert_eq!(got.events.len(), 3);

        let recent = store.recent(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, id);
    }
}
