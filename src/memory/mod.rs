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
//!
//! Module layout (YYC-111): `schema` carries the DDL constant and migration
//! helpers, `codec` carries the message (de)serialization helpers, and this
//! file holds `SessionStore` plus its CRUD surface.

use std::sync::Mutex;

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};

use crate::provider::Message;

mod codec;
mod schema;

#[cfg(test)]
mod tests;

#[cfg(feature = "gateway")]
pub(crate) use schema::{DbPool, open_gateway_pool};
#[cfg(all(test, feature = "gateway"))]
pub(crate) use schema::in_memory_gateway_pool;

use codec::{decode_message, encode_message};
use schema::{initialize_conn, upsert_session_metadata, upsert_session_provider_profile};

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
    /// Active named provider profile when this session was last saved
    /// (`None` means the legacy unnamed `[provider]` block, YYC-95).
    pub provider_profile: Option<String>,
    /// First user-message content, truncated for the picker synopsis.
    pub preview: Option<String>,
}

impl SessionStore {
    /// Open (or create) the session store at `~/.vulcan/sessions.db`. Panics
    /// on fatal DB initialization errors — matches the existing pattern in
    /// `AgentBuilder::build` (api key, provider).
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

    /// Persist (or clear) the named provider profile active for a session
    /// (YYC-95). `None` flags the session as running on the legacy unnamed
    /// `[provider]` block. The session row is created if it doesn't exist
    /// yet so the column is set even before the first message saves.
    pub fn save_provider_profile(
        &self,
        session_id: &str,
        provider_profile: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().timestamp();
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("session DB poisoned"))?;
        upsert_session_provider_profile(&conn, session_id, now, provider_profile)
    }

    /// Read the persisted active provider profile for a session, if any
    /// (YYC-95). Returns `Ok(None)` when the session row doesn't exist or
    /// the column is NULL — both interpretations mean "use the legacy
    /// `[provider]` block".
    pub fn load_provider_profile(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("session DB poisoned"))?;
        let value = conn
            .query_row(
                "SELECT provider_profile FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        Ok(value)
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
                    s.provider_profile,
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
                provider_profile: row.get(5)?,
                message_count: row.get::<_, i64>(6)? as usize,
                preview: row
                    .get::<_, Option<String>>(7)?
                    .map(|s| s.chars().take(60).collect::<String>().replace('\n', " ")),
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

    #[doc(hidden)]
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

