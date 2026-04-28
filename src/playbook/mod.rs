//! YYC-187: project playbooks + task memory.
//!
//! Playbooks are workspace-scoped, user-inspectable bundles of
//! "how this project wants to work" — test commands, branching
//! flows, architecture landmarks, known pitfalls. Entries carry
//! source attribution and a propose → accept lifecycle so the
//! agent can suggest additions without silently rewriting
//! durable state.
//!
//! ## Scope of this PR
//!
//! - `PlaybookSection` enum + `PlaybookEntry` record with status
//!   (`Proposed` / `Accepted`) and `source`.
//! - `Playbook` aggregate keyed by workspace.
//! - `PlaybookStore` trait + in-memory and SQLite backends.
//! - Round-trip tests including the propose → accept transition.
//!
//! ## Deliberately deferred
//!
//! - `vulcan playbook` CLI surface (next PR).
//! - Auto-import from `AGENTS.md` / `CLAUDE.md` / README.
//! - Agent-side suggestion hook (BeforeAgentEnd).
//! - Context-pack integration so playbook entries feed prompts.

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybookSection {
    Setup,
    Build,
    Workflow,
    Architecture,
    Verification,
    Pitfalls,
    Docs,
    Release,
    AgentBehavior,
}

impl PlaybookSection {
    pub fn as_str(self) -> &'static str {
        match self {
            PlaybookSection::Setup => "setup",
            PlaybookSection::Build => "build",
            PlaybookSection::Workflow => "workflow",
            PlaybookSection::Architecture => "architecture",
            PlaybookSection::Verification => "verification",
            PlaybookSection::Pitfalls => "pitfalls",
            PlaybookSection::Docs => "docs",
            PlaybookSection::Release => "release",
            PlaybookSection::AgentBehavior => "agent_behavior",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "setup" => Some(PlaybookSection::Setup),
            "build" => Some(PlaybookSection::Build),
            "workflow" => Some(PlaybookSection::Workflow),
            "architecture" => Some(PlaybookSection::Architecture),
            "verification" => Some(PlaybookSection::Verification),
            "pitfalls" => Some(PlaybookSection::Pitfalls),
            "docs" => Some(PlaybookSection::Docs),
            "release" => Some(PlaybookSection::Release),
            "agent_behavior" | "agent-behavior" => Some(PlaybookSection::AgentBehavior),
            _ => None,
        }
    }
}

/// Lifecycle of a single entry. Agent-suggested additions land
/// `Proposed`; the user explicitly transitions to `Accepted`
/// before any prompt-injection path can use them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    Proposed,
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybookEntry {
    pub id: Uuid,
    pub section: PlaybookSection,
    pub body: String,
    /// Free-form attribution: filename, doc reference, "agent
    /// suggestion run <run_id>", etc.
    pub source: String,
    pub status: EntryStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl PlaybookEntry {
    pub fn proposed(
        section: PlaybookSection,
        body: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            section,
            body: body.into(),
            source: source.into(),
            status: EntryStatus::Proposed,
            created_at: chrono::Utc::now(),
        }
    }

    pub fn accept(&mut self) {
        self.status = EntryStatus::Accepted;
    }
}

/// Storage backend abstraction so callers can use the in-memory
/// impl in tests + SQLite in production without touching writer
/// code paths.
pub trait PlaybookStore: Send + Sync {
    fn upsert_entry(&self, workspace: &str, entry: &PlaybookEntry) -> Result<()>;
    fn list_entries(&self, workspace: &str) -> Result<Vec<PlaybookEntry>>;
    fn accept_entry(&self, workspace: &str, entry_id: Uuid) -> Result<bool>;
    fn remove_entry(&self, workspace: &str, entry_id: Uuid) -> Result<bool>;
}

#[derive(Debug, Default)]
pub struct InMemoryPlaybookStore {
    inner: Mutex<Vec<(String, PlaybookEntry)>>,
}

impl InMemoryPlaybookStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PlaybookStore for InMemoryPlaybookStore {
    fn upsert_entry(&self, workspace: &str, entry: &PlaybookEntry) -> Result<()> {
        let mut guard = self.inner.lock();
        if let Some(slot) = guard
            .iter_mut()
            .find(|(w, e)| w == workspace && e.id == entry.id)
        {
            slot.1 = entry.clone();
        } else {
            guard.push((workspace.to_string(), entry.clone()));
        }
        Ok(())
    }

    fn list_entries(&self, workspace: &str) -> Result<Vec<PlaybookEntry>> {
        Ok(self
            .inner
            .lock()
            .iter()
            .filter(|(w, _)| w == workspace)
            .map(|(_, e)| e.clone())
            .collect())
    }

    fn accept_entry(&self, workspace: &str, entry_id: Uuid) -> Result<bool> {
        let mut guard = self.inner.lock();
        for (w, e) in guard.iter_mut() {
            if w == workspace && e.id == entry_id {
                e.status = EntryStatus::Accepted;
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn remove_entry(&self, workspace: &str, entry_id: Uuid) -> Result<bool> {
        let mut guard = self.inner.lock();
        let before = guard.len();
        guard.retain(|(w, e)| !(w == workspace && e.id == entry_id));
        Ok(guard.len() != before)
    }
}

/// SQLite-backed store at `~/.vulcan/playbooks.db`.
pub struct SqlitePlaybookStore {
    conn: Mutex<Connection>,
}

impl SqlitePlaybookStore {
    pub fn try_new() -> Result<Self> {
        let dir = crate::config::vulcan_home();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create vulcan_home at {}", dir.display()))?;
        Self::try_open_at(&dir.join("playbooks.db"))
    }

    pub fn try_open_at(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("open playbooks DB at {}", path.display()))?;
        Self::initialize(&conn)
            .with_context(|| format!("init playbooks schema at {}", path.display()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn try_open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory playbooks DB")?;
        Self::initialize(&conn).context("init in-memory playbooks schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn initialize(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS playbook_entries (
                id          TEXT PRIMARY KEY,
                workspace   TEXT NOT NULL,
                section     TEXT NOT NULL,
                body        TEXT NOT NULL,
                source      TEXT NOT NULL,
                status      TEXT NOT NULL,
                created_at  TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_playbook_workspace
                ON playbook_entries(workspace, created_at ASC);
            "#,
        )?;
        Ok(())
    }
}

impl PlaybookStore for SqlitePlaybookStore {
    fn upsert_entry(&self, workspace: &str, entry: &PlaybookEntry) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO playbook_entries (id, workspace, section, body, source, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                workspace = excluded.workspace,
                section = excluded.section,
                body = excluded.body,
                source = excluded.source,
                status = excluded.status,
                created_at = excluded.created_at",
            params![
                entry.id.to_string(),
                workspace,
                entry.section.as_str(),
                entry.body,
                entry.source,
                match entry.status {
                    EntryStatus::Proposed => "proposed",
                    EntryStatus::Accepted => "accepted",
                },
                entry.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    fn list_entries(&self, workspace: &str) -> Result<Vec<PlaybookEntry>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, section, body, source, status, created_at
             FROM playbook_entries WHERE workspace = ?1 ORDER BY created_at ASC",
        )?;
        let rows: Vec<PlaybookEntry> = stmt
            .query_map(params![workspace], |row| {
                let id: String = row.get(0)?;
                let section: String = row.get(1)?;
                let status: String = row.get(4)?;
                let created_at: String = row.get(5)?;
                Ok((
                    id,
                    section,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    status,
                    created_at,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(
                |(id, section, body, source, status, created_at)| -> Result<PlaybookEntry> {
                    Ok(PlaybookEntry {
                        id: Uuid::parse_str(&id)?,
                        section: PlaybookSection::parse(&section)
                            .ok_or_else(|| anyhow::anyhow!("unknown section `{section}`"))?,
                        body,
                        source,
                        status: match status.as_str() {
                            "accepted" => EntryStatus::Accepted,
                            _ => EntryStatus::Proposed,
                        },
                        created_at: chrono::DateTime::parse_from_rfc3339(&created_at)?
                            .with_timezone(&chrono::Utc),
                    })
                },
            )
            .collect::<Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn accept_entry(&self, workspace: &str, entry_id: Uuid) -> Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE playbook_entries SET status = 'accepted' WHERE workspace = ?1 AND id = ?2",
            params![workspace, entry_id.to_string()],
        )?;
        Ok(n > 0)
    }

    fn remove_entry(&self, workspace: &str, entry_id: Uuid) -> Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "DELETE FROM playbook_entries WHERE workspace = ?1 AND id = ?2",
            params![workspace, entry_id.to_string()],
        )?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(section: PlaybookSection, body: &str, source: &str) -> PlaybookEntry {
        PlaybookEntry::proposed(section, body, source)
    }

    #[test]
    fn proposed_entries_must_be_explicitly_accepted() {
        let mut e = entry(PlaybookSection::Build, "cargo test", "agent");
        assert_eq!(e.status, EntryStatus::Proposed);
        e.accept();
        assert_eq!(e.status, EntryStatus::Accepted);
    }

    #[test]
    fn in_memory_store_isolates_workspaces() {
        let store = InMemoryPlaybookStore::new();
        let a = entry(PlaybookSection::Build, "cargo test", "AGENTS.md");
        let b = entry(PlaybookSection::Build, "make test", "README.md");
        store.upsert_entry("ws-a", &a).unwrap();
        store.upsert_entry("ws-b", &b).unwrap();
        let listed_a = store.list_entries("ws-a").unwrap();
        assert_eq!(listed_a.len(), 1);
        assert_eq!(listed_a[0].body, "cargo test");
        let listed_b = store.list_entries("ws-b").unwrap();
        assert_eq!(listed_b.len(), 1);
        assert_eq!(listed_b[0].body, "make test");
    }

    #[test]
    fn accept_entry_round_trips_through_sqlite() {
        let store = SqlitePlaybookStore::try_open_in_memory().unwrap();
        let mut e = entry(PlaybookSection::Verification, "cargo clippy", "AGENTS.md");
        let id = e.id;
        store.upsert_entry("ws", &e).unwrap();
        assert!(store.accept_entry("ws", id).unwrap());
        let listed = store.list_entries("ws").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, EntryStatus::Accepted);
        // Idempotency: a second accept call returns true (still
        // exists) without error.
        e.accept();
        store.upsert_entry("ws", &e).unwrap();
        assert_eq!(
            store.list_entries("ws").unwrap()[0].status,
            EntryStatus::Accepted
        );
    }

    #[test]
    fn sqlite_store_lists_in_creation_order() {
        let store = SqlitePlaybookStore::try_open_in_memory().unwrap();
        let e1 = entry(PlaybookSection::Setup, "cargo build", "first");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let e2 = entry(
            PlaybookSection::Pitfalls,
            "PTY tests need /usr/bin/bash",
            "second",
        );
        store.upsert_entry("ws", &e1).unwrap();
        store.upsert_entry("ws", &e2).unwrap();
        let listed = store.list_entries("ws").unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].source, "first");
        assert_eq!(listed[1].source, "second");
    }

    #[test]
    fn remove_returns_false_when_entry_missing() {
        let store = SqlitePlaybookStore::try_open_in_memory().unwrap();
        let removed = store.remove_entry("ws", Uuid::new_v4()).unwrap();
        assert!(!removed);
    }

    #[test]
    fn section_round_trips_through_string() {
        for s in [
            PlaybookSection::Setup,
            PlaybookSection::Build,
            PlaybookSection::Workflow,
            PlaybookSection::Architecture,
            PlaybookSection::Verification,
            PlaybookSection::Pitfalls,
            PlaybookSection::Docs,
            PlaybookSection::Release,
            PlaybookSection::AgentBehavior,
        ] {
            assert_eq!(PlaybookSection::parse(s.as_str()).unwrap(), s);
        }
        assert!(PlaybookSection::parse("nope").is_none());
    }
}
