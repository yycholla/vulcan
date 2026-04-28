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
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
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
    /// Implementation plan, phased spec, task breakdown.
    Plan,
    /// Proposed or applied code change, patch summary, file set.
    Diff,
    /// Review report, audit, diagnostic bundle, benchmark.
    Report,
    /// Structured output from a long-running or important tool call.
    ToolOutput,
    /// Final handoff text from a spawned subagent.
    SubagentSummary,
    /// Bounded diagnostic excerpt; relies on `redaction` metadata
    /// to flag any sensitivity.
    LogExcerpt,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::Plan => "plan",
            ArtifactKind::Diff => "diff",
            ArtifactKind::Report => "report",
            ArtifactKind::ToolOutput => "tool_output",
            ArtifactKind::SubagentSummary => "subagent_summary",
            ArtifactKind::LogExcerpt => "log_excerpt",
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
    /// Inline payload. UTF-8 — non-text payloads should ride
    /// `external_path` instead of being base64-stuffed here.
    pub content: Option<String>,
    /// Filesystem path to the payload (relative paths interpreted
    /// against the cwd at read time). Reserved for when inline
    /// would exceed `INLINE_MAX_BYTES`.
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
            content: Some(content.into()),
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

    pub fn with_redaction(mut self, tag: impl Into<String>) -> Self {
        self.redaction = RedactionTag(Some(tag.into()));
        self
    }

    pub fn with_parent(mut self, parent: ArtifactId) -> Self {
        self.parent_artifact_id = Some(parent);
        self
    }
}

pub trait ArtifactStore: Send + Sync {
    fn create(&self, artifact: &Artifact) -> Result<()>;
    fn get(&self, id: ArtifactId) -> Result<Option<Artifact>>;
    fn list_for_run(&self, run_id: crate::run_record::RunId) -> Result<Vec<Artifact>>;
    fn list_for_session(&self, session_id: &str) -> Result<Vec<Artifact>>;
    fn recent(&self, limit: usize) -> Result<Vec<Artifact>>;
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

impl ArtifactStore for InMemoryArtifactStore {
    fn create(&self, artifact: &Artifact) -> Result<()> {
        self.inner.lock().push(artifact.clone());
        Ok(())
    }

    fn get(&self, id: ArtifactId) -> Result<Option<Artifact>> {
        Ok(self.inner.lock().iter().find(|a| a.id == id).cloned())
    }

    fn list_for_run(&self, run_id: crate::run_record::RunId) -> Result<Vec<Artifact>> {
        Ok(self
            .inner
            .lock()
            .iter()
            .filter(|a| a.run_id == Some(run_id))
            .cloned()
            .collect())
    }

    fn list_for_session(&self, session_id: &str) -> Result<Vec<Artifact>> {
        Ok(self
            .inner
            .lock()
            .iter()
            .filter(|a| a.session_id.as_deref() == Some(session_id))
            .cloned()
            .collect())
    }

    fn recent(&self, limit: usize) -> Result<Vec<Artifact>> {
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
                content             TEXT,
                external_path       TEXT,
                redaction           TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_artifacts_run_id ON artifacts(run_id);
            CREATE INDEX IF NOT EXISTS idx_artifacts_session_id ON artifacts(session_id);
            CREATE INDEX IF NOT EXISTS idx_artifacts_created_at ON artifacts(created_at DESC);
            "#,
        )?;
        Ok(())
    }

    // YYC-275: 11 fields come straight from a SQLite row decode;
    // collapsing them into a struct here would just rename the
    // problem at the call site. Allowed at this site only.
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
        content: Option<String>,
        external_path: Option<String>,
        redaction: Option<String>,
    ) -> Result<Artifact> {
        let kind = match kind.as_str() {
            "plan" => ArtifactKind::Plan,
            "diff" => ArtifactKind::Diff,
            "report" => ArtifactKind::Report,
            "tool_output" => ArtifactKind::ToolOutput,
            "subagent_summary" => ArtifactKind::SubagentSummary,
            "log_excerpt" => ArtifactKind::LogExcerpt,
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
            content,
            external_path,
            redaction: RedactionTag(redaction),
        })
    }
}

impl ArtifactStore for SqliteArtifactStore {
    fn create(&self, artifact: &Artifact) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO artifacts (id, kind, run_id, session_id, parent_artifact_id, source,
                                    created_at, title, content, external_path, redaction)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                artifact.id.to_string(),
                artifact.kind.as_str(),
                artifact.run_id.map(|r| r.to_string()),
                artifact.session_id,
                artifact.parent_artifact_id.map(|p| p.to_string()),
                artifact.source,
                artifact.created_at.to_rfc3339(),
                artifact.title,
                artifact.content,
                artifact.external_path,
                artifact.redaction.0,
            ],
        )?;
        Ok(())
    }

    fn get(&self, id: ArtifactId) -> Result<Option<Artifact>> {
        let conn = self.conn.lock();
        let row = conn
            .query_row(
                "SELECT id, kind, run_id, session_id, parent_artifact_id, source,
                        created_at, title, content, external_path, redaction
                 FROM artifacts WHERE id = ?1",
                params![id.to_string()],
                row_columns,
            )
            .optional()?;
        match row {
            None => Ok(None),
            Some(t) => Ok(Some(Self::row_to_artifact(
                t.0, t.1, t.2, t.3, t.4, t.5, t.6, t.7, t.8, t.9, t.10,
            )?)),
        }
    }

    fn list_for_run(&self, run_id: crate::run_record::RunId) -> Result<Vec<Artifact>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, kind, run_id, session_id, parent_artifact_id, source,
                    created_at, title, content, external_path, redaction
             FROM artifacts WHERE run_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows: Vec<_> = stmt
            .query_map(params![run_id.to_string()], row_columns)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|t| Self::row_to_artifact(t.0, t.1, t.2, t.3, t.4, t.5, t.6, t.7, t.8, t.9, t.10))
            .collect()
    }

    fn list_for_session(&self, session_id: &str) -> Result<Vec<Artifact>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, kind, run_id, session_id, parent_artifact_id, source,
                    created_at, title, content, external_path, redaction
             FROM artifacts WHERE session_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows: Vec<_> = stmt
            .query_map(params![session_id], row_columns)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|t| Self::row_to_artifact(t.0, t.1, t.2, t.3, t.4, t.5, t.6, t.7, t.8, t.9, t.10))
            .collect()
    }

    fn recent(&self, limit: usize) -> Result<Vec<Artifact>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, kind, run_id, session_id, parent_artifact_id, source,
                    created_at, title, content, external_path, redaction
             FROM artifacts ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows: Vec<_> = stmt
            .query_map(params![limit as i64], row_columns)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|t| Self::row_to_artifact(t.0, t.1, t.2, t.3, t.4, t.5, t.6, t.7, t.8, t.9, t.10))
            .collect()
    }
}

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
);

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
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_kind_round_trips_string() {
        for k in [
            ArtifactKind::Plan,
            ArtifactKind::Diff,
            ArtifactKind::Report,
            ArtifactKind::ToolOutput,
            ArtifactKind::SubagentSummary,
            ArtifactKind::LogExcerpt,
        ] {
            let s = k.as_str();
            assert!(!s.contains(' '));
        }
    }

    #[test]
    fn in_memory_store_round_trips_text_artifact() {
        let store = InMemoryArtifactStore::new();
        let art = Artifact::inline_text(ArtifactKind::Plan, "## Plan\n- step 1")
            .with_session_id("sess-7")
            .with_source("planner");
        let id = art.id;
        store.create(&art).unwrap();
        let got = store.get(id).unwrap().unwrap();
        assert_eq!(got.kind, ArtifactKind::Plan);
        assert_eq!(got.content.as_deref(), Some("## Plan\n- step 1"));
        assert_eq!(got.session_id.as_deref(), Some("sess-7"));
    }

    #[test]
    fn sqlite_store_round_trips_two_kinds() {
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
        store.create(&plan).unwrap();

        let diff = Artifact::inline_text(
            ArtifactKind::Diff,
            r#"{"path":"src/foo.rs","added":3,"removed":2}"#,
        )
        .with_session_id(session)
        .with_source("write_file")
        .with_redaction("path-only");
        let diff_id = diff.id;
        store.create(&diff).unwrap();

        let listed = store.list_for_session(session).unwrap();
        assert_eq!(listed.len(), 2);

        let got_plan = store.get(plan_id).unwrap().unwrap();
        assert_eq!(got_plan.title.as_deref(), Some("rollout plan"));
        assert_eq!(got_plan.kind, ArtifactKind::Plan);

        let got_diff = store.get(diff_id).unwrap().unwrap();
        assert_eq!(got_diff.kind, ArtifactKind::Diff);
        assert_eq!(got_diff.source.as_deref(), Some("write_file"));
        assert_eq!(got_diff.redaction.0.as_deref(), Some("path-only"));
    }

    #[test]
    fn sqlite_store_links_artifact_to_run_id() {
        let store = SqliteArtifactStore::try_open_in_memory().unwrap();
        let run = crate::run_record::RunId::new();
        let art = Artifact::inline_text(ArtifactKind::Report, "ok").with_run_id(run);
        store.create(&art).unwrap();
        let listed = store.list_for_run(run).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].run_id, Some(run));
    }

    #[test]
    fn sqlite_store_recent_returns_newest_first() {
        let store = SqliteArtifactStore::try_open_in_memory().unwrap();
        let a1 = Artifact::inline_text(ArtifactKind::Plan, "a");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let a2 = Artifact::inline_text(ArtifactKind::Plan, "b");
        store.create(&a1).unwrap();
        store.create(&a2).unwrap();
        let recent = store.recent(10).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, a2.id);
        assert_eq!(recent[1].id, a1.id);
    }

    #[test]
    fn parent_artifact_id_round_trips() {
        let store = SqliteArtifactStore::try_open_in_memory().unwrap();
        let parent = Artifact::inline_text(ArtifactKind::Plan, "parent");
        let parent_id = parent.id;
        store.create(&parent).unwrap();
        let child = Artifact::inline_text(ArtifactKind::Diff, "child").with_parent(parent_id);
        let child_id = child.id;
        store.create(&child).unwrap();
        let got = store.get(child_id).unwrap().unwrap();
        assert_eq!(got.parent_artifact_id, Some(parent_id));
    }
}
