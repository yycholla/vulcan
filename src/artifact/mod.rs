//! YYC-180: typed artifact system for durable agent outputs.
//!
//! Plans, diffs, reports, tool outputs, subagent summaries, log
//! excerpts — anything that should outlive a single chat message
//! and be referenced from runs, sessions, tools, and future
//! extensions. Artifacts ride alongside run records (YYC-179) but
//! own their own SQLite store so size limits and lifecycle policy
//! can diverge from the timeline.
//!
//! ## Scope of this PR
//!
//! - `ArtifactId` newtype + `ArtifactKind` enum.
//! - `Artifact` record + `ArtifactStore` trait.
//! - In-memory and SQLite backends.
//! - Inline `content` column for small payloads (≤ 256 KiB);
//!   `external_path` for the rare big ones — codified, but the
//!   actual large-payload tier waits on the first real consumer.
//! - Redaction + provenance metadata fields so callers don't have
//!   to invent their own.
//!
//! ## Deliberately deferred
//!
//! - Agent / tool / hook integration that *creates* artifacts (the
//!   `RunEvent::ArtifactCreated` placeholder lands here as a
//!   reference target).
//! - `vulcan artifact list/show` CLI surface.
//! - TUI render hooks.
//! - Garbage collection / retention policy.
//! - Cross-run artifact references (parent → child) at the gateway.

use anyhow::{Context, Result};
use async_trait::async_trait;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

/// Stable identifier for a single artifact. Distinct from `RunId`
/// so an artifact can outlive (or even pre-date) the run that
/// referenced it — useful for replay flows that synthesize
/// artifacts from saved transcripts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ArtifactId(Uuid);

impl ArtifactId {
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

impl Default for ArtifactId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// First-pass artifact taxonomy. Matches the issue's initial set —
/// new variants land alongside the consumer that needs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// Plain text that is durable but not necessarily a full report.
    Text,
    /// Implementation plan, phased spec, task breakdown.
    Plan,
    /// Proposed or applied code patch.
    Patch,
    /// Backward-compatible tag for existing diff artifacts.
    Diff,
    /// Filesystem/object-store file reference.
    File,
    Image,
    Audio,
    Video,
    Table,
    Json,
    Log,
    /// Review report, audit, diagnostic bundle, benchmark.
    Report,
    /// Structured output from a long-running or important tool call.
    ToolOutput,
    /// Final handoff text from a spawned subagent.
    SubagentSummary,
    /// Bounded diagnostic excerpt; relies on `redaction` metadata
    /// to flag any sensitivity.
    LogExcerpt,
    Other,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::Text => "text",
            ArtifactKind::Plan => "plan",
            ArtifactKind::Patch => "patch",
            ArtifactKind::Diff => "diff",
            ArtifactKind::File => "file",
            ArtifactKind::Image => "image",
            ArtifactKind::Audio => "audio",
            ArtifactKind::Video => "video",
            ArtifactKind::Table => "table",
            ArtifactKind::Json => "json",
            ArtifactKind::Log => "log",
            ArtifactKind::Report => "report",
            ArtifactKind::ToolOutput => "tool_output",
            ArtifactKind::SubagentSummary => "subagent_summary",
            ArtifactKind::LogExcerpt => "log_excerpt",
            ArtifactKind::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactVisibility {
    Conversation,
    Workspace,
    Private,
    Sensitive,
}

impl Default for ArtifactVisibility {
    fn default() -> Self {
        Self::Conversation
    }
}

impl ArtifactVisibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Workspace => "workspace",
            Self::Private => "private",
            Self::Sensitive => "sensitive",
        }
    }

    fn from_str(raw: &str) -> Result<Self> {
        match raw {
            "conversation" => Ok(Self::Conversation),
            "workspace" => Ok(Self::Workspace),
            "private" => Ok(Self::Private),
            "sensitive" => Ok(Self::Sensitive),
            other => anyhow::bail!("unknown artifact visibility `{other}`"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRetention {
    Session,
    Workspace,
    Manual,
    Ephemeral,
}

impl Default for ArtifactRetention {
    fn default() -> Self {
        Self::Session
    }
}

impl ArtifactRetention {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Workspace => "workspace",
            Self::Manual => "manual",
            Self::Ephemeral => "ephemeral",
        }
    }

    fn from_str(raw: &str) -> Result<Self> {
        match raw {
            "session" => Ok(Self::Session),
            "workspace" => Ok(Self::Workspace),
            "manual" => Ok(Self::Manual),
            "ephemeral" => Ok(Self::Ephemeral),
            other => anyhow::bail!("unknown artifact retention `{other}`"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactReplaySafety {
    Safe,
    SummaryOnly,
    Unsafe,
    Unknown,
}

impl Default for ArtifactReplaySafety {
    fn default() -> Self {
        Self::Unknown
    }
}

impl ArtifactReplaySafety {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::SummaryOnly => "summary_only",
            Self::Unsafe => "unsafe",
            Self::Unknown => "unknown",
        }
    }

    fn from_str(raw: &str) -> Result<Self> {
        match raw {
            "safe" => Ok(Self::Safe),
            "summary_only" => Ok(Self::SummaryOnly),
            "unsafe" => Ok(Self::Unsafe),
            "unknown" => Ok(Self::Unknown),
            other => anyhow::bail!("unknown artifact replay safety `{other}`"),
        }
    }
}

/// Tag describing what redaction was applied (if any) before the
/// artifact landed. `None` means the caller didn't opt into
/// redaction; `Some("secrets-masked")` etc. tells reviewers what
/// to expect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct RedactionTag(pub Option<String>);

/// One artifact record. `content` carries small inline payloads;
/// `external_path` carries a filesystem reference for larger ones.
/// At most one of the two should be set on creation — the codec
/// stores both columns either way so backends stay symmetric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub kind: ArtifactKind,
    /// Run that produced this artifact, if any. Optional because
    /// `vulcan replay` and offline test fixtures synthesize
    /// artifacts without a live run.
    pub run_id: Option<crate::run_record::RunId>,
    pub session_id: Option<String>,
    pub parent_artifact_id: Option<ArtifactId>,
    /// Free-form provenance string — typically the tool/hook/agent
    /// name that produced the artifact. Useful for `vulcan artifact
    /// list --source <name>` once that ships.
    pub source: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Optional human-friendly label.
    pub title: Option<String>,
    pub mime_type: Option<String>,
    pub schema: Option<String>,
    /// Durable pointer to the payload. This may be content-addressed,
    /// a local artifact-store URI, or an external file path depending
    /// on the producer. It is safe to store in run records and CLI/TUI
    /// metadata because raw payload bytes stay out of this field.
    pub storage_uri: Option<String>,
    /// Hash of the payload before storage/redaction. Safe for equality
    /// checks and replay diagnostics; never stores raw payload text.
    pub content_hash: Option<String>,
    pub size_bytes: Option<u64>,
    pub provenance: Option<String>,
    pub visibility: ArtifactVisibility,
    pub retention: ArtifactRetention,
    pub replay_safety: ArtifactReplaySafety,
    /// Inline payload. UTF-8 — callers must opt into this explicitly;
    /// use [`Artifact::metadata_from_payload`] when the content should
    /// not be copied into durable metadata by default.
    pub content: Option<String>,
    /// Filesystem path to the payload (relative paths interpreted
    /// against the cwd at read time). Kept for compatibility with older
    /// consumers; new code should prefer `storage_uri`.
    pub external_path: Option<String>,
    pub redaction: RedactionTag,
}

/// Soft cap on inline payload size. Anything bigger should land in
/// `external_path` so the SQLite row stays small. Picked at 256
/// KiB — well above plan/report sizes, well below the SQLite
/// per-row default (1 GiB).
pub const INLINE_MAX_BYTES: usize = 256 * 1024;

impl Artifact {
    pub fn inline_text(kind: ArtifactKind, content: impl Into<String>) -> Self {
        Self {
            id: ArtifactId::new(),
            kind,
            run_id: None,
            session_id: None,
            parent_artifact_id: None,
            source: None,
            created_at: chrono::Utc::now(),
            title: None,
            mime_type: Some("text/plain; charset=utf-8".into()),
            schema: None,
            storage_uri: None,
            content_hash: None,
            size_bytes: None,
            provenance: None,
            visibility: ArtifactVisibility::Conversation,
            retention: ArtifactRetention::Session,
            replay_safety: ArtifactReplaySafety::SummaryOnly,
            content: Some(content.into()),
            external_path: None,
            redaction: RedactionTag::default(),
        }
    }

    pub fn metadata_from_payload(
        kind: ArtifactKind,
        payload: &[u8],
        storage_uri: impl Into<String>,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(payload);
        let digest = hasher.finalize();
        let mut hash = String::with_capacity(7 + digest.len() * 2);
        hash.push_str("sha256:");
        for byte in digest.iter() {
            hash.push_str(&format!("{:02x}", byte));
        }
        Self {
            id: ArtifactId::new(),
            kind,
            run_id: None,
            session_id: None,
            parent_artifact_id: None,
            source: None,
            created_at: chrono::Utc::now(),
            title: None,
            mime_type: None,
            schema: None,
            storage_uri: Some(storage_uri.into()),
            content_hash: Some(hash),
            size_bytes: Some(payload.len() as u64),
            provenance: None,
            visibility: ArtifactVisibility::Conversation,
            retention: ArtifactRetention::Session,
            replay_safety: ArtifactReplaySafety::Unknown,
            content: None,
            external_path: None,
            redaction: RedactionTag::default(),
        }
    }

    pub fn with_run_id(mut self, run_id: crate::run_record::RunId) -> Self {
        self.run_id = Some(run_id);
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }

    pub fn with_schema(mut self, schema: impl Into<String>) -> Self {
        self.schema = Some(schema.into());
        self
    }

    pub fn with_storage_uri(mut self, storage_uri: impl Into<String>) -> Self {
        self.storage_uri = Some(storage_uri.into());
        self
    }

    pub fn with_provenance(mut self, provenance: impl Into<String>) -> Self {
        self.provenance = Some(provenance.into());
        self
    }

    pub fn with_visibility(mut self, visibility: ArtifactVisibility) -> Self {
        self.visibility = visibility;
        self
    }

    pub fn with_retention(mut self, retention: ArtifactRetention) -> Self {
        self.retention = retention;
        self
    }

    pub fn with_replay_safety(mut self, replay_safety: ArtifactReplaySafety) -> Self {
        self.replay_safety = replay_safety;
        self
    }

    pub fn with_redaction(mut self, tag: impl Into<String>) -> Self {
        self.redaction = RedactionTag(Some(tag.into()));
        self
    }

    pub fn with_parent(mut self, parent: ArtifactId) -> Self {
        self.parent_artifact_id = Some(parent);
        self
    }
}

#[async_trait]
pub trait ArtifactStore: Send + Sync {
    async fn create(&self, artifact: &Artifact) -> Result<()>;
    async fn get(&self, id: ArtifactId) -> Result<Option<Artifact>>;
    async fn list_for_run(&self, run_id: crate::run_record::RunId) -> Result<Vec<Artifact>>;
    async fn list_for_session(&self, session_id: &str) -> Result<Vec<Artifact>>;
    async fn recent(&self, limit: usize) -> Result<Vec<Artifact>>;
}

/// Open the default artifact store. Selects the backend at compile
/// time: Turso under `turso-backend` (GH #704), else rusqlite.
pub async fn open_default_store() -> Result<Arc<dyn ArtifactStore>> {
    #[cfg(feature = "turso-backend")]
    {
        Ok(Arc::new(TursoArtifactStore::try_new().await?))
    }
    #[cfg(not(feature = "turso-backend"))]
    {
        Ok(Arc::new(SqliteArtifactStore::try_new()?))
    }
}

#[derive(Debug, Default)]
pub struct InMemoryArtifactStore {
    inner: Mutex<Vec<Artifact>>,
}

impl InMemoryArtifactStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ArtifactStore for InMemoryArtifactStore {
    async fn create(&self, artifact: &Artifact) -> Result<()> {
        self.inner.lock().push(artifact.clone());
        Ok(())
    }

    async fn get(&self, id: ArtifactId) -> Result<Option<Artifact>> {
        Ok(self.inner.lock().iter().find(|a| a.id == id).cloned())
    }

    async fn list_for_run(&self, run_id: crate::run_record::RunId) -> Result<Vec<Artifact>> {
        Ok(self
            .inner
            .lock()
            .iter()
            .filter(|a| a.run_id == Some(run_id))
            .cloned()
            .collect())
    }

    async fn list_for_session(&self, session_id: &str) -> Result<Vec<Artifact>> {
        Ok(self
            .inner
            .lock()
            .iter()
            .filter(|a| a.session_id.as_deref() == Some(session_id))
            .cloned()
            .collect())
    }

    async fn recent(&self, limit: usize) -> Result<Vec<Artifact>> {
        Ok(self
            .inner
            .lock()
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect())
    }
}

/// SQLite-backed store at `~/.vulcan/artifacts.db`. Schema mirrors
/// the [`Artifact`] struct field-for-field — readers stay simple,
/// migrations rare. Kind is stored as the `as_str` tag so adding
/// a variant is backward compatible.
pub struct SqliteArtifactStore {
    conn: Mutex<Connection>,
}

impl SqliteArtifactStore {
    pub fn try_new() -> Result<Self> {
        let dir = crate::config::vulcan_home();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create vulcan_home at {}", dir.display()))?;
        Self::try_open_at(&dir.join("artifacts.db"))
    }

    pub fn try_open_at(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("open artifacts DB at {}", path.display()))?;
        Self::initialize(&conn)
            .with_context(|| format!("init artifacts schema at {}", path.display()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn try_open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory artifacts DB")?;
        Self::initialize(&conn).context("init in-memory artifacts schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn initialize(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS artifacts (
                id                  TEXT PRIMARY KEY,
                kind                TEXT NOT NULL,
                run_id              TEXT,
                session_id          TEXT,
                parent_artifact_id  TEXT,
                source              TEXT,
                created_at          TEXT NOT NULL,
                title               TEXT,
                mime_type           TEXT,
                schema              TEXT,
                storage_uri         TEXT,
                content_hash        TEXT,
                size_bytes          INTEGER,
                provenance          TEXT,
                visibility          TEXT NOT NULL DEFAULT 'conversation',
                retention           TEXT NOT NULL DEFAULT 'session',
                replay_safety       TEXT NOT NULL DEFAULT 'unknown',
                content             TEXT,
                external_path       TEXT,
                redaction           TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_artifacts_run_id ON artifacts(run_id);
            CREATE INDEX IF NOT EXISTS idx_artifacts_session_id ON artifacts(session_id);
            CREATE INDEX IF NOT EXISTS idx_artifacts_created_at ON artifacts(created_at DESC);
            "#,
        )?;

        let columns = artifact_columns(conn)?;
        for (name, ddl) in [
            (
                "mime_type",
                "ALTER TABLE artifacts ADD COLUMN mime_type TEXT",
            ),
            ("schema", "ALTER TABLE artifacts ADD COLUMN schema TEXT"),
            (
                "storage_uri",
                "ALTER TABLE artifacts ADD COLUMN storage_uri TEXT",
            ),
            (
                "content_hash",
                "ALTER TABLE artifacts ADD COLUMN content_hash TEXT",
            ),
            (
                "size_bytes",
                "ALTER TABLE artifacts ADD COLUMN size_bytes INTEGER",
            ),
            (
                "provenance",
                "ALTER TABLE artifacts ADD COLUMN provenance TEXT",
            ),
            (
                "visibility",
                "ALTER TABLE artifacts ADD COLUMN visibility TEXT NOT NULL DEFAULT 'conversation'",
            ),
            (
                "retention",
                "ALTER TABLE artifacts ADD COLUMN retention TEXT NOT NULL DEFAULT 'session'",
            ),
            (
                "replay_safety",
                "ALTER TABLE artifacts ADD COLUMN replay_safety TEXT NOT NULL DEFAULT 'unknown'",
            ),
        ] {
            if !columns.iter().any(|c| c == name) {
                conn.execute_batch(ddl)?;
            }
        }
        Ok(())
    }

    // YYC-275: fields come straight from a SQLite row decode; collapsing
    // them into a struct here would just rename the problem at the call site.
    #[allow(clippy::too_many_arguments)]
    fn row_to_artifact(
        id: String,
        kind: String,
        run_id: Option<String>,
        session_id: Option<String>,
        parent: Option<String>,
        source: Option<String>,
        created_at: String,
        title: Option<String>,
        mime_type: Option<String>,
        schema: Option<String>,
        storage_uri: Option<String>,
        content_hash: Option<String>,
        size_bytes: Option<i64>,
        provenance: Option<String>,
        visibility: String,
        retention: String,
        replay_safety: String,
        content: Option<String>,
        external_path: Option<String>,
        redaction: Option<String>,
    ) -> Result<Artifact> {
        let kind = match kind.as_str() {
            "text" => ArtifactKind::Text,
            "plan" => ArtifactKind::Plan,
            "patch" => ArtifactKind::Patch,
            "diff" => ArtifactKind::Diff,
            "file" => ArtifactKind::File,
            "image" => ArtifactKind::Image,
            "audio" => ArtifactKind::Audio,
            "video" => ArtifactKind::Video,
            "table" => ArtifactKind::Table,
            "json" => ArtifactKind::Json,
            "log" => ArtifactKind::Log,
            "report" => ArtifactKind::Report,
            "tool_output" => ArtifactKind::ToolOutput,
            "subagent_summary" => ArtifactKind::SubagentSummary,
            "log_excerpt" => ArtifactKind::LogExcerpt,
            "other" => ArtifactKind::Other,
            other => anyhow::bail!("unknown artifact kind `{other}`"),
        };
        Ok(Artifact {
            id: ArtifactId::from_uuid(Uuid::parse_str(&id)?),
            kind,
            run_id: run_id
                .map(|s| Uuid::parse_str(&s).map(crate::run_record::RunId::from_uuid))
                .transpose()?,
            session_id,
            parent_artifact_id: parent
                .map(|s| Uuid::parse_str(&s).map(ArtifactId::from_uuid))
                .transpose()?,
            source,
            created_at: chrono::DateTime::parse_from_rfc3339(&created_at)?
                .with_timezone(&chrono::Utc),
            title,
            mime_type,
            schema,
            storage_uri,
            content_hash,
            size_bytes: size_bytes.map(|n| n.max(0) as u64),
            provenance,
            visibility: ArtifactVisibility::from_str(&visibility)?,
            retention: ArtifactRetention::from_str(&retention)?,
            replay_safety: ArtifactReplaySafety::from_str(&replay_safety)?,
            content,
            external_path,
            redaction: RedactionTag(redaction),
        })
    }
}

#[async_trait]
impl ArtifactStore for SqliteArtifactStore {
    async fn create(&self, artifact: &Artifact) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO artifacts (id, kind, run_id, session_id, parent_artifact_id, source,
                                    created_at, title, mime_type, schema, storage_uri,
                                    content_hash, size_bytes, provenance, visibility, retention,
                                    replay_safety, content, external_path, redaction)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                artifact.id.to_string(),
                artifact.kind.as_str(),
                artifact.run_id.map(|r| r.to_string()),
                artifact.session_id,
                artifact.parent_artifact_id.map(|p| p.to_string()),
                artifact.source,
                artifact.created_at.to_rfc3339(),
                artifact.title,
                artifact.mime_type,
                artifact.schema,
                artifact.storage_uri,
                artifact.content_hash,
                artifact.size_bytes.map(|n| n as i64),
                artifact.provenance,
                artifact.visibility.as_str(),
                artifact.retention.as_str(),
                artifact.replay_safety.as_str(),
                artifact.content,
                artifact.external_path,
                artifact.redaction.0,
            ],
        )?;
        Ok(())
    }

    async fn get(&self, id: ArtifactId) -> Result<Option<Artifact>> {
        let conn = self.conn.lock();
        let row = conn
            .query_row(
                artifact_select_sql("WHERE id = ?1").as_str(),
                params![id.to_string()],
                row_columns,
            )
            .optional()?;
        match row {
            None => Ok(None),
            Some(t) => Ok(Some(row_tuple_to_artifact(t)?)),
        }
    }

    async fn list_for_run(&self, run_id: crate::run_record::RunId) -> Result<Vec<Artifact>> {
        let conn = self.conn.lock();
        let sql = artifact_select_sql("WHERE run_id = ?1 ORDER BY created_at ASC");
        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<_> = stmt
            .query_map(params![run_id.to_string()], row_columns)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(row_tuple_to_artifact).collect()
    }

    async fn list_for_session(&self, session_id: &str) -> Result<Vec<Artifact>> {
        let conn = self.conn.lock();
        let sql = artifact_select_sql("WHERE session_id = ?1 ORDER BY created_at ASC");
        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<_> = stmt
            .query_map(params![session_id], row_columns)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(row_tuple_to_artifact).collect()
    }

    async fn recent(&self, limit: usize) -> Result<Vec<Artifact>> {
        let conn = self.conn.lock();
        let sql = artifact_select_sql("ORDER BY created_at DESC LIMIT ?1");
        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<_> = stmt
            .query_map(params![limit as i64], row_columns)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(row_tuple_to_artifact).collect()
    }
}

/// Turso-backed artifact store (GH #704). Async; bare
/// `turso::Connection`, no `Mutex`/r2d2/`spawn_blocking`. Fresh DBs
/// only, so it skips the rusqlite path's additive-column migration.
#[cfg(feature = "turso-backend")]
pub struct TursoArtifactStore {
    conn: turso::Connection,
}

#[cfg(feature = "turso-backend")]
impl TursoArtifactStore {
    pub async fn try_new() -> Result<Self> {
        let path = crate::config::vulcan_home().join("artifacts.db");
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
            "CREATE TABLE IF NOT EXISTS artifacts (
                id TEXT PRIMARY KEY, kind TEXT NOT NULL, run_id TEXT, session_id TEXT,
                parent_artifact_id TEXT, source TEXT, created_at TEXT NOT NULL, title TEXT,
                mime_type TEXT, schema TEXT, storage_uri TEXT, content_hash TEXT,
                size_bytes INTEGER, provenance TEXT,
                visibility TEXT NOT NULL DEFAULT 'conversation',
                retention TEXT NOT NULL DEFAULT 'session',
                replay_safety TEXT NOT NULL DEFAULT 'unknown',
                content TEXT, external_path TEXT, redaction TEXT
            )",
            (),
        )
        .await?;
        for idx in [
            "CREATE INDEX IF NOT EXISTS idx_artifacts_run_id ON artifacts(run_id)",
            "CREATE INDEX IF NOT EXISTS idx_artifacts_session_id ON artifacts(session_id)",
            "CREATE INDEX IF NOT EXISTS idx_artifacts_created_at ON artifacts(created_at DESC)",
        ] {
            conn.execute(idx, ()).await?;
        }
        Ok(())
    }

    async fn read_rows(&self, sql: &str, params: impl turso::IntoParams) -> Result<Vec<Artifact>> {
        let mut rows = self.conn.query(sql, params).await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let t: ArtifactRowColumns = (
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
                row.get(12)?,
                row.get(13)?,
                row.get(14)?,
                row.get(15)?,
                row.get(16)?,
                row.get(17)?,
                row.get(18)?,
                row.get(19)?,
            );
            out.push(row_tuple_to_artifact(t)?);
        }
        Ok(out)
    }
}

#[cfg(feature = "turso-backend")]
#[async_trait]
impl ArtifactStore for TursoArtifactStore {
    async fn create(&self, a: &Artifact) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO artifacts (id, kind, run_id, session_id, parent_artifact_id, \
                 source, created_at, title, mime_type, schema, storage_uri, content_hash, \
                 size_bytes, provenance, visibility, retention, replay_safety, content, \
                 external_path, redaction) VALUES \
                 (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
                turso::params_from_iter([
                    turso::Value::from(a.id.to_string()),
                    turso::Value::from(a.kind.as_str().to_string()),
                    a.run_id.map(|r| r.to_string()).into(),
                    a.session_id.clone().into(),
                    a.parent_artifact_id.map(|p| p.to_string()).into(),
                    a.source.clone().into(),
                    turso::Value::from(a.created_at.to_rfc3339()),
                    a.title.clone().into(),
                    a.mime_type.clone().into(),
                    a.schema.clone().into(),
                    a.storage_uri.clone().into(),
                    a.content_hash.clone().into(),
                    a.size_bytes.map(|n| n as i64).into(),
                    a.provenance.clone().into(),
                    turso::Value::from(a.visibility.as_str().to_string()),
                    turso::Value::from(a.retention.as_str().to_string()),
                    turso::Value::from(a.replay_safety.as_str().to_string()),
                    a.content.clone().into(),
                    a.external_path.clone().into(),
                    a.redaction.0.clone().into(),
                ]),
            )
            .await?;
        Ok(())
    }

    async fn get(&self, id: ArtifactId) -> Result<Option<Artifact>> {
        let sql = artifact_select_sql("WHERE id = ?1");
        Ok(self
            .read_rows(&sql, (id.to_string(),))
            .await?
            .into_iter()
            .next())
    }

    async fn list_for_run(&self, run_id: crate::run_record::RunId) -> Result<Vec<Artifact>> {
        let sql = artifact_select_sql("WHERE run_id = ?1 ORDER BY created_at ASC");
        self.read_rows(&sql, (run_id.to_string(),)).await
    }

    async fn list_for_session(&self, session_id: &str) -> Result<Vec<Artifact>> {
        let sql = artifact_select_sql("WHERE session_id = ?1 ORDER BY created_at ASC");
        self.read_rows(&sql, (session_id.to_string(),)).await
    }

    async fn recent(&self, limit: usize) -> Result<Vec<Artifact>> {
        let sql = artifact_select_sql("ORDER BY created_at DESC LIMIT ?1");
        self.read_rows(&sql, (limit as i64,)).await
    }
}

#[allow(clippy::type_complexity)]
type ArtifactRowColumns = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn artifact_columns(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("PRAGMA table_info(artifacts)")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn artifact_select_sql(suffix: &str) -> String {
    format!(
        "SELECT id, kind, run_id, session_id, parent_artifact_id, source,
                created_at, title, mime_type, schema, storage_uri, content_hash,
                size_bytes, provenance, visibility, retention, replay_safety,
                content, external_path, redaction
         FROM artifacts {suffix}"
    )
}

fn row_tuple_to_artifact(t: ArtifactRowColumns) -> Result<Artifact> {
    SqliteArtifactStore::row_to_artifact(
        t.0, t.1, t.2, t.3, t.4, t.5, t.6, t.7, t.8, t.9, t.10, t.11, t.12, t.13, t.14, t.15, t.16,
        t.17, t.18, t.19,
    )
}

fn row_columns(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtifactRowColumns> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
        row.get(16)?,
        row.get(17)?,
        row.get(18)?,
        row.get(19)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_kind_round_trips_string() {
        for k in [
            ArtifactKind::Text,
            ArtifactKind::Plan,
            ArtifactKind::Patch,
            ArtifactKind::Diff,
            ArtifactKind::File,
            ArtifactKind::Image,
            ArtifactKind::Audio,
            ArtifactKind::Video,
            ArtifactKind::Table,
            ArtifactKind::Json,
            ArtifactKind::Log,
            ArtifactKind::Report,
            ArtifactKind::ToolOutput,
            ArtifactKind::SubagentSummary,
            ArtifactKind::LogExcerpt,
            ArtifactKind::Other,
        ] {
            let s = k.as_str();
            assert!(!s.contains(' '));
        }
    }

    #[tokio::test]
    async fn in_memory_store_round_trips_text_artifact() {
        let store = InMemoryArtifactStore::new();
        let art = Artifact::inline_text(ArtifactKind::Plan, "## Plan\n- step 1")
            .with_session_id("sess-7")
            .with_source("planner");
        let id = art.id;
        store.create(&art).await.unwrap();
        let got = store.get(id).await.unwrap().unwrap();
        assert_eq!(got.kind, ArtifactKind::Plan);
        assert_eq!(got.content.as_deref(), Some("## Plan\n- step 1"));
        assert_eq!(got.session_id.as_deref(), Some("sess-7"));
    }

    #[test]
    fn metadata_payload_records_hash_size_and_safe_storage_uri_without_raw_content() {
        let payload = "API_KEY=secret123\nsummary: safe to persist";
        let art = Artifact::metadata_from_payload(
            ArtifactKind::Report,
            payload.as_bytes(),
            "artifact://sha256/report-1",
        )
        .with_title("safe report")
        .with_source("review")
        .with_redaction("secrets-masked")
        .with_provenance("tool:review")
        .with_visibility(ArtifactVisibility::Private)
        .with_retention(ArtifactRetention::Workspace)
        .with_replay_safety(ArtifactReplaySafety::SummaryOnly)
        .with_mime_type("text/markdown")
        .with_schema("vulcan.report.v1");

        assert!(
            art.content.is_none(),
            "raw payload must not be stored by default"
        );
        assert_eq!(art.size_bytes, Some(payload.len() as u64));
        assert!(art.content_hash.as_deref().unwrap().starts_with("sha256:"));
        assert_eq!(
            art.storage_uri.as_deref(),
            Some("artifact://sha256/report-1")
        );
        assert_eq!(art.visibility, ArtifactVisibility::Private);
        assert_eq!(art.retention, ArtifactRetention::Workspace);
        assert_eq!(art.replay_safety, ArtifactReplaySafety::SummaryOnly);
        let serialized = serde_json::to_string(&art).unwrap();
        assert!(!serialized.contains("secret123"));
    }

    #[tokio::test]
    async fn sqlite_store_persists_contract_metadata_without_raw_payload() {
        let store = SqliteArtifactStore::try_open_in_memory().unwrap();
        let run = crate::run_record::RunId::new();
        let payload = br#"{"token":"secret-token","rows":3}"#;
        let art = Artifact::metadata_from_payload(
            ArtifactKind::Json,
            payload,
            "artifact://sha256/table-1",
        )
        .with_run_id(run)
        .with_title("tool json")
        .with_source("tool:query")
        .with_redaction("secret-values-redacted")
        .with_provenance("tool:query args=sha256:abc")
        .with_visibility(ArtifactVisibility::Workspace)
        .with_retention(ArtifactRetention::Session)
        .with_replay_safety(ArtifactReplaySafety::Safe)
        .with_mime_type("application/json")
        .with_schema("vulcan.tool-output.v1");
        let id = art.id;

        store.create(&art).await.unwrap();
        let got = store.get(id).await.unwrap().unwrap();

        assert_eq!(got.run_id, Some(run));
        assert_eq!(got.kind, ArtifactKind::Json);
        assert_eq!(got.content, None);
        assert_eq!(
            got.storage_uri.as_deref(),
            Some("artifact://sha256/table-1")
        );
        assert_eq!(got.size_bytes, Some(payload.len() as u64));
        assert!(got.content_hash.as_deref().unwrap().starts_with("sha256:"));
        assert_eq!(got.visibility, ArtifactVisibility::Workspace);
        assert_eq!(got.retention, ArtifactRetention::Session);
        assert_eq!(got.replay_safety, ArtifactReplaySafety::Safe);
        assert_eq!(got.mime_type.as_deref(), Some("application/json"));
        assert_eq!(got.schema.as_deref(), Some("vulcan.tool-output.v1"));
        assert_eq!(
            got.provenance.as_deref(),
            Some("tool:query args=sha256:abc")
        );
        assert!(
            !serde_json::to_string(&got)
                .unwrap()
                .contains("secret-token")
        );
    }

    #[tokio::test]
    async fn sqlite_store_round_trips_two_kinds() {
        // Acceptance: at least two artifact kinds exercised — one
        // text-like (Plan), one structured (Diff with JSON-shaped
        // content).
        let store = SqliteArtifactStore::try_open_in_memory().unwrap();
        let session = "sess-1";

        let plan = Artifact::inline_text(ArtifactKind::Plan, "phase 1: scaffold\nphase 2: ship")
            .with_session_id(session)
            .with_title("rollout plan")
            .with_redaction("none");
        let plan_id = plan.id;
        store.create(&plan).await.unwrap();

        let diff = Artifact::inline_text(
            ArtifactKind::Diff,
            r#"{"path":"src/foo.rs","added":3,"removed":2}"#,
        )
        .with_session_id(session)
        .with_source("write_file")
        .with_redaction("path-only");
        let diff_id = diff.id;
        store.create(&diff).await.unwrap();

        let listed = store.list_for_session(session).await.unwrap();
        assert_eq!(listed.len(), 2);

        let got_plan = store.get(plan_id).await.unwrap().unwrap();
        assert_eq!(got_plan.title.as_deref(), Some("rollout plan"));
        assert_eq!(got_plan.kind, ArtifactKind::Plan);

        let got_diff = store.get(diff_id).await.unwrap().unwrap();
        assert_eq!(got_diff.kind, ArtifactKind::Diff);
        assert_eq!(got_diff.source.as_deref(), Some("write_file"));
        assert_eq!(got_diff.redaction.0.as_deref(), Some("path-only"));
    }

    #[tokio::test]
    async fn sqlite_store_links_artifact_to_run_id() {
        let store = SqliteArtifactStore::try_open_in_memory().unwrap();
        let run = crate::run_record::RunId::new();
        let art = Artifact::inline_text(ArtifactKind::Report, "ok").with_run_id(run);
        store.create(&art).await.unwrap();
        let listed = store.list_for_run(run).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].run_id, Some(run));
    }

    #[tokio::test]
    async fn sqlite_store_recent_returns_newest_first() {
        let store = SqliteArtifactStore::try_open_in_memory().unwrap();
        let a1 = Artifact::inline_text(ArtifactKind::Plan, "a");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let a2 = Artifact::inline_text(ArtifactKind::Plan, "b");
        store.create(&a1).await.unwrap();
        store.create(&a2).await.unwrap();
        let recent = store.recent(10).await.unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, a2.id);
        assert_eq!(recent[1].id, a1.id);
    }

    #[tokio::test]
    async fn parent_artifact_id_round_trips() {
        let store = SqliteArtifactStore::try_open_in_memory().unwrap();
        let parent = Artifact::inline_text(ArtifactKind::Plan, "parent");
        let parent_id = parent.id;
        store.create(&parent).await.unwrap();
        let child = Artifact::inline_text(ArtifactKind::Diff, "child").with_parent(parent_id);
        let child_id = child.id;
        store.create(&child).await.unwrap();
        let got = store.get(child_id).await.unwrap().unwrap();
        assert_eq!(got.parent_artifact_id, Some(parent_id));
    }
}

// GH #704: the Turso backend satisfies the same ArtifactStore contract.
#[cfg(all(test, feature = "turso-backend"))]
mod turso_tests {
    use super::*;

    #[tokio::test]
    async fn turso_store_round_trips_create_get_list_recent() {
        let store = TursoArtifactStore::try_open_in_memory().await.unwrap();
        let run_id = crate::run_record::RunId::new();
        let art = Artifact::inline_text(ArtifactKind::Report, "findings")
            .with_source("review")
            .with_title("Review report")
            .with_session_id("sess-1")
            .with_run_id(run_id);
        let id = art.id;
        store.create(&art).await.unwrap();

        let got = store.get(id).await.unwrap().expect("artifact present");
        assert_eq!(got.kind, ArtifactKind::Report);
        assert_eq!(got.source.as_deref(), Some("review"));
        assert_eq!(got.content.as_deref(), Some("findings"));

        assert_eq!(store.list_for_run(run_id).await.unwrap().len(), 1);
        assert_eq!(store.list_for_session("sess-1").await.unwrap().len(), 1);
        assert_eq!(store.recent(10).await.unwrap().len(), 1);
    }
}
