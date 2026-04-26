//! Persistent session storage backed by SQLite (with FTS5 for cross-session
//! search).
//!
//! Schema:
//! - `sessions(id PK, created_at, last_active, parent_session_id, lineage_label)`
//! - `messages(id PK AUTOINCREMENT, session_id FK, position, role, content,
//!    tool_call_id, tool_calls_json, created_at)` indexed on `(session_id, position)`
//! - `messages_fts` — external-content FTS5 over `messages.content`, kept in
//!    sync via insert/update/delete triggers.
//! - `inbound_queue(id PK, platform, chat_id, user_id, text, received_at,
//!    attempts, state)` — gateway daemon ingress, indexed by lane+state and state+received_at.
//! - `outbound_queue(id PK, platform, chat_id, text, attachments_json, enqueued_at,
//!    next_attempt_at, attempts, state, last_error)` — gateway daemon egress, indexed by state+next_attempt_at.
//!
//! The JSONL format used previously is gone; old data under
//! `~/.vulcan/sessions/*.jsonl` is left in place but not read. Migration is a
//! manual one-off if it ever matters (see Linear YYC-14).

use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};

use crate::provider::{Message, ToolCall};

const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS sessions (
    id                TEXT PRIMARY KEY,
    created_at        INTEGER NOT NULL,
    last_active       INTEGER NOT NULL,
    parent_session_id TEXT,
    lineage_label     TEXT
);

CREATE TABLE IF NOT EXISTS messages (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id        TEXT NOT NULL,
    position          INTEGER NOT NULL,
    role              TEXT NOT NULL,
    content           TEXT,
    tool_call_id      TEXT,
    tool_calls_json   TEXT,
    reasoning_content TEXT,
    created_at        INTEGER NOT NULL,
    FOREIGN KEY(session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, position);

CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    content,
    content='messages',
    content_rowid='id'
);

CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, content) VALUES (new.id, COALESCE(new.content, ''));
END;

CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.id, COALESCE(old.content, ''));
END;

CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.id, COALESCE(old.content, ''));
    INSERT INTO messages_fts(rowid, content) VALUES (new.id, COALESCE(new.content, ''));
END;

CREATE TABLE IF NOT EXISTS inbound_queue (
  id INTEGER PRIMARY KEY,
  platform TEXT NOT NULL,
  chat_id  TEXT NOT NULL,
  user_id  TEXT NOT NULL,
  text     TEXT NOT NULL,
  received_at INTEGER NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  state    TEXT NOT NULL  -- 'pending'|'processing'|'failed'
);
CREATE INDEX IF NOT EXISTS idx_inbound_lane  ON inbound_queue(platform, chat_id, state);
CREATE INDEX IF NOT EXISTS idx_inbound_state ON inbound_queue(state, received_at);

CREATE TABLE IF NOT EXISTS outbound_queue (
  id INTEGER PRIMARY KEY,
  platform TEXT NOT NULL,
  chat_id  TEXT NOT NULL,
  text     TEXT NOT NULL,
  attachments_json TEXT NOT NULL DEFAULT '[]',
  enqueued_at INTEGER NOT NULL,
  next_attempt_at INTEGER NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0,
  state    TEXT NOT NULL,  -- 'pending'|'sending'|'failed'
  last_error TEXT
);
CREATE INDEX IF NOT EXISTS idx_outbound_due ON outbound_queue(state, next_attempt_at);
"#;

pub struct SessionStore {
    conn: Mutex<Connection>,
}

/// One row from a full-text search. Score is the BM25 rank (lower = better
/// per FTS5 conventions).
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub session_id: String,
    pub position: i64,
    pub role: String,
    pub content: String,
    pub created_at: i64,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub created_at: i64,
    pub last_active: i64,
    pub message_count: usize,
    pub parent_session_id: Option<String>,
    pub lineage_label: Option<String>,
    /// First user-message content, truncated for the picker synopsis.
    pub preview: Option<String>,
}

impl SessionStore {
    /// Open (or create) the session store at `~/.vulcan/sessions.db`. Panics
    /// on fatal DB initialization errors — matches the existing pattern in
    /// `Agent::new` (api key, provider).
    pub fn new() -> Self {
        let dir = crate::config::vulcan_home();
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("sessions.db");

        let conn = Connection::open(&path)
            .unwrap_or_else(|e| panic!("Failed to open session DB at {}: {e}", path.display()));

        initialize_conn(&conn)
            .unwrap_or_else(|e| panic!("Failed to initialize session DB schema: {e}"));

        Self {
            conn: Mutex::new(conn),
        }
    }

    /// Most recently active session, by `last_active`. `None` if there are no
    /// sessions yet.
    pub fn last_session_id(&self) -> Option<String> {
        let conn = self.conn.lock().ok()?;
        conn.query_row(
            "SELECT id FROM sessions ORDER BY last_active DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Load all messages for `session_id` in the order they were saved.
    /// Returns `Ok(None)` if the session doesn't exist.
    pub fn load_history(&self, session_id: &str) -> Result<Option<Vec<Message>>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("session DB poisoned"))?;

        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM sessions WHERE id = ?1",
                params![session_id],
                |_| Ok(true),
            )
            .optional()
            .context("Failed to check session existence")?
            .unwrap_or(false);
        if !exists {
            return Ok(None);
        }

        let mut stmt = conn.prepare(
            "SELECT role, content, tool_call_id, tool_calls_json, reasoning_content
             FROM messages
             WHERE session_id = ?1
             ORDER BY position ASC",
        )?;

        let rows = stmt.query_map(params![session_id], |row| {
            let role: String = row.get(0)?;
            let content: Option<String> = row.get(1)?;
            let tool_call_id: Option<String> = row.get(2)?;
            let tool_calls_json: Option<String> = row.get(3)?;
            let reasoning_content: Option<String> = row.get(4)?;
            Ok((
                role,
                content,
                tool_call_id,
                tool_calls_json,
                reasoning_content,
            ))
        })?;

        let mut messages = Vec::new();
        for row in rows {
            let (role, content, tool_call_id, tool_calls_json, reasoning_content) = row?;
            let msg = decode_message(
                &role,
                content,
                tool_call_id,
                tool_calls_json,
                reasoning_content,
            )?;
            messages.push(msg);
        }

        Ok(Some(messages))
    }

    /// Save the full message history for `session_id`. The session row is
    /// upserted (`last_active` bumped); existing messages for the session are
    /// deleted and replaced with `messages` — full-snapshot semantics matching
    /// the per-prompt save the agent emits.
    ///
    /// Prefer `append_messages` when only new messages need saving — this
    /// does a full DELETE + re-INSERT which is O(n) in the total message count.
    pub fn save_messages(&self, session_id: &str, messages: &[Message]) -> Result<()> {
        let now = Utc::now().timestamp();

        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("session DB poisoned"))?;
        let tx = conn.transaction()?;

        // Upsert the session row while preserving any previously-recorded
        // lineage metadata.
        upsert_session_metadata(&tx, session_id, now, None, None)?;

        // Replace all messages for this session — full snapshot semantics.
        tx.execute(
            "DELETE FROM messages WHERE session_id = ?1",
            params![session_id],
        )?;

        for (idx, msg) in messages.iter().enumerate() {
            let (role, content, tool_call_id, tool_calls_json, reasoning_content) =
                encode_message(msg)?;
            tx.execute(
                "INSERT INTO messages
                 (session_id, position, role, content, tool_call_id, tool_calls_json, reasoning_content, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    session_id,
                    idx as i64,
                    role,
                    content,
                    tool_call_id,
                    tool_calls_json,
                    reasoning_content,
                    now,
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// Append new messages to a session — no DELETE, no full-snapshot
    /// overhead. Finds the current max position for the session and inserts
    /// from there. Use this from the agent loop to avoid O(n) delete+reinsert
    /// on every turn.
    pub fn append_messages(&self, session_id: &str, messages: &[Message]) -> Result<()> {
        let now = Utc::now().timestamp();
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("session DB poisoned"))?;
        let tx = conn.transaction()?;

        upsert_session_metadata(&tx, session_id, now, None, None)?;

        let next_pos: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM messages WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .unwrap_or(0);

        for (offset, msg) in messages.iter().enumerate() {
            let (role, content, tool_call_id, tool_calls_json, reasoning_content) =
                encode_message(msg)?;
            tx.execute(
                "INSERT INTO messages
                 (session_id, position, role, content, tool_call_id, tool_calls_json, reasoning_content, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    session_id,
                    next_pos + offset as i64,
                    role,
                    content,
                    tool_call_id,
                    tool_calls_json,
                    reasoning_content,
                    now,
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// Persist session metadata even before any messages exist. Used to create
    /// truthful child sessions with lineage before the first turn lands.
    pub fn save_session_metadata(
        &self,
        session_id: &str,
        parent_session_id: Option<&str>,
        lineage_label: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().timestamp();
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("session DB poisoned"))?;

        upsert_session_metadata(&conn, session_id, now, parent_session_id, lineage_label)
    }

    /// Most-recent-first list of saved sessions, capped at `limit`.
    pub fn list_sessions(&self, limit: usize) -> Result<Vec<SessionSummary>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("session DB poisoned"))?;

        let mut stmt = conn.prepare(
            "SELECT s.id, s.created_at, s.last_active, s.parent_session_id, s.lineage_label,
                    (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id) AS msg_count,
                    (SELECT content FROM messages m WHERE m.session_id = s.id AND m.role = 'user'
                     ORDER BY m.position ASC LIMIT 1) AS preview
             FROM sessions s
             ORDER BY s.last_active DESC
             LIMIT ?1",
        )?;

        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(SessionSummary {
                id: row.get(0)?,
                created_at: row.get(1)?,
                last_active: row.get(2)?,
                parent_session_id: row.get(3)?,
                lineage_label: row.get(4)?,
                message_count: row.get::<_, i64>(5)? as usize,
                preview: row.get::<_, Option<String>>(6)?.map(|s| {
                    s.chars().take(60).collect::<String>().replace('\n', " ")
                }),
            })
        })?;

        let mut summaries = Vec::new();
        for row in rows {
            summaries.push(row?);
        }
        Ok(summaries)
    }

    /// Full-text search across every saved message. Returns the top `limit`
    /// hits ranked by BM25.
    pub fn search_messages(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("session DB poisoned"))?;

        let mut stmt = conn.prepare(
            "SELECT m.session_id, m.position, m.role, m.content, m.created_at, bm25(messages_fts) AS score
             FROM messages_fts
             JOIN messages m ON m.id = messages_fts.rowid
             WHERE messages_fts MATCH ?1
             ORDER BY score
             LIMIT ?2",
        )?;

        let rows = stmt.query_map(params![query, limit as i64], |row| {
            Ok(SearchHit {
                session_id: row.get(0)?,
                position: row.get(1)?,
                role: row.get(2)?,
                content: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                created_at: row.get(4)?,
                score: row.get(5)?,
            })
        })?;

        let mut hits = Vec::new();
        for row in rows {
            hits.push(row?);
        }
        Ok(hits)
    }

    #[cfg(any(test, feature = "bench-soak"))]
    pub fn in_memory() -> Self {
        let conn = Connection::open_in_memory().expect("open in-memory session DB");
        initialize_conn(&conn).expect("initialize in-memory session DB");
        Self {
            conn: Mutex::new(conn),
        }
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::new()
    }
}

fn initialize_conn(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;

    // Idempotent migrations for DBs created before additive columns landed.
    let _ = conn.execute("ALTER TABLE messages ADD COLUMN reasoning_content TEXT", []);
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN parent_session_id TEXT", []);
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN lineage_label TEXT", []);
    Ok(())
}

#[cfg(test)]
pub(crate) fn initialize_test_conn(conn: &Connection) -> Result<()> {
    initialize_conn(conn)
}

fn upsert_session_metadata(
    conn: &Connection,
    session_id: &str,
    now: i64,
    parent_session_id: Option<&str>,
    lineage_label: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions (id, created_at, last_active, parent_session_id, lineage_label)
         VALUES (?1, ?2, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET
             last_active = excluded.last_active,
             parent_session_id = COALESCE(excluded.parent_session_id, sessions.parent_session_id),
             lineage_label = COALESCE(excluded.lineage_label, sessions.lineage_label)",
        params![session_id, now, parent_session_id, lineage_label],
    )?;
    Ok(())
}

type Encoded = (
    &'static str,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn encode_message(msg: &Message) -> Result<Encoded> {
    Ok(match msg {
        Message::System { content } => ("system", Some(content.clone()), None, None, None),
        Message::User { content } => ("user", Some(content.clone()), None, None, None),
        Message::Assistant {
            content,
            tool_calls,
            reasoning_content,
        } => {
            let tool_calls_json = match tool_calls {
                Some(tcs) => Some(serde_json::to_string(tcs).context("encode tool_calls")?),
                None => None,
            };
            (
                "assistant",
                content.clone(),
                None,
                tool_calls_json,
                reasoning_content.clone(),
            )
        }
        Message::Tool {
            tool_call_id,
            content,
        } => (
            "tool",
            Some(content.clone()),
            Some(tool_call_id.clone()),
            None,
            None,
        ),
    })
}

fn decode_message(
    role: &str,
    content: Option<String>,
    tool_call_id: Option<String>,
    tool_calls_json: Option<String>,
    reasoning_content: Option<String>,
) -> Result<Message> {
    Ok(match role {
        "system" => Message::System {
            content: content.unwrap_or_default(),
        },
        "user" => Message::User {
            content: content.unwrap_or_default(),
        },
        "assistant" => {
            let tool_calls = match tool_calls_json {
                Some(s) => {
                    Some(serde_json::from_str::<Vec<ToolCall>>(&s).context("decode tool_calls")?)
                }
                None => None,
            };
            Message::Assistant {
                content,
                tool_calls,
                reasoning_content,
            }
        }
        "tool" => Message::Tool {
            tool_call_id: tool_call_id.unwrap_or_default(),
            content: content.unwrap_or_default(),
        },
        other => anyhow::bail!("unknown role in DB: {other}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store_in(dir: &TempDir) -> SessionStore {
        // Bypass `new()` so we don't write into ~/.vulcan during tests.
        let path = dir.path().join("sessions.db");
        let conn = Connection::open(&path).unwrap();
        initialize_conn(&conn).unwrap();
        SessionStore {
            conn: Mutex::new(conn),
        }
    }

    #[test]
    fn round_trip_messages() {
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        let session_id = uuid::Uuid::new_v4().to_string();

        let messages = vec![
            Message::System {
                content: "you are a helpful agent".into(),
            },
            Message::User {
                content: "what is rust?".into(),
            },
            Message::Assistant {
                content: Some("a systems language with strong types".into()),
                tool_calls: None,
                reasoning_content: None,
            },
        ];

        store.save_messages(&session_id, &messages).unwrap();
        let loaded = store.load_history(&session_id).unwrap().unwrap();
        assert_eq!(loaded.len(), 3);
        match &loaded[1] {
            Message::User { content } => assert_eq!(content, "what is rust?"),
            other => panic!("expected User, got {other:?}"),
        }
    }

    #[test]
    fn last_session_id_returns_most_recent() {
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        let id = uuid::Uuid::new_v4().to_string();

        store
            .save_messages(
                &id,
                &[Message::User {
                    content: "first".into(),
                }],
            )
            .unwrap();
        assert_eq!(store.last_session_id(), Some(id));
    }

    #[test]
    fn list_sessions_returns_summaries_in_recency_order() {
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        let s1 = uuid::Uuid::new_v4().to_string();
        let s2 = uuid::Uuid::new_v4().to_string();

        store
            .save_messages(
                &s1,
                &[Message::User {
                    content: "a".into(),
                }],
            )
            .unwrap();
        // Sleep 1s would make this deterministic, but the second save bumps
        // last_active beyond the first's wall-clock-second granularity in
        // practice. Make it explicit by saving twice with different content.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        store
            .save_messages(
                &s2,
                &[Message::User {
                    content: "b".into(),
                }],
            )
            .unwrap();

        let summaries = store.list_sessions(10).unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].id, s2, "most recent should come first");
        assert_eq!(summaries[1].id, s1);
        assert_eq!(summaries[0].message_count, 1);
    }

    #[test]
    fn fts_search_finds_content() {
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        let session_id = uuid::Uuid::new_v4().to_string();

        store
            .save_messages(
                &session_id,
                &[
                    Message::User {
                        content: "the quick brown fox jumps over the lazy dog".into(),
                    },
                    Message::User {
                        content: "lorem ipsum dolor sit amet".into(),
                    },
                ],
            )
            .unwrap();

        let hits = store.search_messages("brown fox", 10).unwrap();
        assert!(
            hits.iter().any(|h| h.content.contains("brown fox")),
            "expected fox hit, got {hits:?}"
        );
    }

    #[test]
    fn session_lineage_survives_metadata_and_message_saves() {
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        let parent_id = uuid::Uuid::new_v4().to_string();
        let child_id = uuid::Uuid::new_v4().to_string();

        store
            .save_messages(
                &parent_id,
                &[Message::User {
                    content: "root".into(),
                }],
            )
            .unwrap();
        store
            .save_session_metadata(
                &child_id,
                Some(&parent_id),
                Some("branched from root session"),
            )
            .unwrap();
        store
            .save_messages(
                &child_id,
                &[Message::User {
                    content: "child".into(),
                }],
            )
            .unwrap();

        let summaries = store.list_sessions(10).unwrap();
        let child = summaries
            .iter()
            .find(|s| s.id == child_id)
            .expect("child summary should exist");
        assert_eq!(child.parent_session_id.as_deref(), Some(parent_id.as_str()));
        assert_eq!(
            child.lineage_label.as_deref(),
            Some("branched from root session")
        );
        assert_eq!(child.message_count, 1);
    }

    #[test]
    fn save_messages_preserves_existing_lineage_metadata() {
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        let parent_id = uuid::Uuid::new_v4().to_string();
        let child_id = uuid::Uuid::new_v4().to_string();

        store
            .save_session_metadata(&child_id, Some(&parent_id), Some("forked"))
            .unwrap();
        store
            .save_messages(
                &child_id,
                &[Message::User {
                    content: "first".into(),
                }],
            )
            .unwrap();
        store
            .save_messages(
                &child_id,
                &[Message::User {
                    content: "second".into(),
                }],
            )
            .unwrap();

        let summaries = store.list_sessions(10).unwrap();
        let child = summaries
            .iter()
            .find(|s| s.id == child_id)
            .expect("child summary should exist");
        assert_eq!(child.parent_session_id.as_deref(), Some(parent_id.as_str()));
        assert_eq!(child.lineage_label.as_deref(), Some("forked"));
        assert_eq!(child.message_count, 1);
    }

    #[test]
    fn assistant_with_tool_calls_round_trips() {
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        let id = uuid::Uuid::new_v4().to_string();

        let messages = vec![Message::Assistant {
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                call_type: "function".into(),
                function: crate::provider::ToolCallFunction {
                    name: "bash".into(),
                    arguments: r#"{"command":"ls"}"#.into(),
                },
            }]),
            reasoning_content: None,
        }];

        store.save_messages(&id, &messages).unwrap();
        let loaded = store.load_history(&id).unwrap().unwrap();

        match &loaded[0] {
            Message::Assistant { tool_calls, .. } => {
                let tcs = tool_calls.as_ref().expect("tool calls present");
                assert_eq!(tcs.len(), 1);
                assert_eq!(tcs[0].function.name, "bash");
            }
            other => panic!("expected Assistant, got {other:?}"),
        }
    }

    #[test]
    fn queue_tables_created() {
        let store = SessionStore::in_memory();
        let conn = store.conn.lock().expect("lock");
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master \
             WHERE type='table' AND name IN ('inbound_queue','outbound_queue')",
                [],
                |r| r.get(0),
            )
            .expect("query");
        assert_eq!(count, 2);
    }

    #[test]
    fn queue_indexes_created() {
        let store = SessionStore::in_memory();
        let conn = store.conn.lock().expect("lock");
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master \
             WHERE type='index' AND name IN ('idx_inbound_lane','idx_inbound_state','idx_outbound_due')",
                [],
                |r| r.get(0),
            )
            .expect("query");
        assert_eq!(count, 3);
    }

    #[test]
    fn reasoning_content_round_trips() {
        let dir = TempDir::new().unwrap();
        let store = store_in(&dir);
        let id = uuid::Uuid::new_v4().to_string();

        let messages = vec![Message::Assistant {
            content: Some("the answer is 42".into()),
            tool_calls: None,
            reasoning_content: Some("First I considered…then I weighed…".into()),
        }];

        store.save_messages(&id, &messages).unwrap();
        let loaded = store.load_history(&id).unwrap().unwrap();
        match &loaded[0] {
            Message::Assistant {
                reasoning_content, ..
            } => assert_eq!(
                reasoning_content.as_deref(),
                Some("First I considered…then I weighed…")
            ),
            other => panic!("expected Assistant, got {other:?}"),
        }
    }
}
