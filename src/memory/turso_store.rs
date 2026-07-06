//! Turso backend for [`SessionStore`] (GH #704). FTS rides Turso's
//! native engine — a `USING fts` index that the engine auto-maintains.
//!
//! Score contract: FTS5's `bm25()` returns *negative, lower = better*;
//! Turso's `fts_score()` returns *positive, higher = better*. This impl
//! negates `fts_score` so `SearchHit.score` keeps the "lower is better"
//! contract downstream consumers (RecallHook's `score <= min_score`
//! filter) already rely on.

use anyhow::{Context, Result};
use chrono::Utc;

use super::codec::{decode_message, encode_message};
use super::{SearchHit, SessionStore, SessionSummary};
use crate::provider::Message;

/// Session schema for Turso.
const SESSION_SCHEMA_TURSO: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS sessions (
        id                TEXT PRIMARY KEY,
        created_at        INTEGER NOT NULL,
        last_active       INTEGER NOT NULL,
        parent_session_id TEXT,
        lineage_label     TEXT,
        provider_profile  TEXT
    )",
    "CREATE TABLE IF NOT EXISTS messages (
        id                INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id        TEXT NOT NULL,
        position          INTEGER NOT NULL,
        role              TEXT NOT NULL,
        content           TEXT,
        tool_call_id      TEXT,
        tool_calls_json   TEXT,
        reasoning_content TEXT,
        created_at        INTEGER NOT NULL
    )",
    "CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, position)",
    "CREATE INDEX IF NOT EXISTS messages_fts ON messages USING fts(content)",
];

impl SessionStore {
    pub async fn try_new() -> Result<Self> {
        let dir = crate::config::vulcan_home();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create vulcan_home at {}", dir.display()))?;
        // Keep the Turso filename distinct until users intentionally
        // move old session history; files touched by Turso FTS are not
        // readable by pre-cutover builds.
        Self::try_open_at(&dir.join("sessions.turso.db")).await
    }

    pub async fn try_open_at(path: &std::path::Path) -> Result<Self> {
        let conn = crate::db::open(path).await?;
        Self::initialize(&conn).await?;
        Ok(Self { conn })
    }

    #[doc(hidden)]
    pub async fn in_memory() -> Self {
        let conn = crate::db::open_in_memory()
            .await
            .expect("open in-memory session DB");
        Self::initialize(&conn)
            .await
            .expect("initialize in-memory session DB");
        Self { conn }
    }

    async fn initialize(conn: &turso::Connection) -> Result<()> {
        for stmt in SESSION_SCHEMA_TURSO {
            crate::db::execute_ddl(conn, stmt).await?;
        }
        Ok(())
    }

    async fn upsert_session_metadata(
        &self,
        session_id: &str,
        now: i64,
        parent_session_id: Option<&str>,
        lineage_label: Option<&str>,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO sessions (id, created_at, last_active, parent_session_id, lineage_label) \
                 VALUES (?1, ?2, ?2, ?3, ?4) \
                 ON CONFLICT(id) DO UPDATE SET \
                     last_active = excluded.last_active, \
                     parent_session_id = COALESCE(excluded.parent_session_id, sessions.parent_session_id), \
                     lineage_label = COALESCE(excluded.lineage_label, sessions.lineage_label)",
                turso::params_from_iter([
                    turso::Value::from(session_id.to_string()),
                    turso::Value::from(now),
                    parent_session_id.map(str::to_string).into(),
                    lineage_label.map(str::to_string).into(),
                ]),
            )
            .await?;
        Ok(())
    }

    async fn insert_message(
        &self,
        session_id: &str,
        position: i64,
        msg: &Message,
        now: i64,
    ) -> Result<()> {
        let (role, content, tool_call_id, tool_calls_json, reasoning_content) =
            encode_message(msg)?;
        self.conn
            .execute(
                "INSERT INTO messages \
                 (session_id, position, role, content, tool_call_id, tool_calls_json, reasoning_content, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                turso::params_from_iter([
                    turso::Value::from(session_id.to_string()),
                    turso::Value::from(position),
                    turso::Value::from(role),
                    content.into(),
                    tool_call_id.into(),
                    tool_calls_json.into(),
                    reasoning_content.into(),
                    turso::Value::from(now),
                ]),
            )
            .await?;
        Ok(())
    }

    pub async fn last_session_id(&self) -> Option<String> {
        let mut rows = self
            .conn
            .query(
                "SELECT id FROM sessions ORDER BY last_active DESC LIMIT 1",
                (),
            )
            .await
            .ok()?;
        let row = rows.next().await.ok()??;
        row.get(0).ok()
    }

    pub async fn load_history(&self, session_id: &str) -> Result<Option<Vec<Message>>> {
        let mut exists_rows = self
            .conn
            .query(
                "SELECT 1 FROM sessions WHERE id = ?1",
                (session_id.to_string(),),
            )
            .await?;
        if exists_rows.next().await?.is_none() {
            return Ok(None);
        }

        let mut rows = self
            .conn
            .query(
                "SELECT role, content, tool_call_id, tool_calls_json, reasoning_content \
                 FROM messages WHERE session_id = ?1 ORDER BY position ASC",
                (session_id.to_string(),),
            )
            .await?;
        let mut messages = Vec::new();
        while let Some(row) = rows.next().await? {
            let role: String = row.get(0)?;
            let content: Option<String> = row.get(1)?;
            let tool_call_id: Option<String> = row.get(2)?;
            let tool_calls_json: Option<String> = row.get(3)?;
            let reasoning_content: Option<String> = row.get(4)?;
            messages.push(decode_message(
                &role,
                content,
                tool_call_id,
                tool_calls_json,
                reasoning_content,
            )?);
        }
        Ok(Some(messages))
    }

    pub async fn save_messages(&self, session_id: &str, messages: &[Message]) -> Result<()> {
        let now = Utc::now().timestamp();
        self.upsert_session_metadata(session_id, now, None, None)
            .await?;
        self.conn
            .execute(
                "DELETE FROM messages WHERE session_id = ?1",
                (session_id.to_string(),),
            )
            .await?;
        for (idx, msg) in messages.iter().enumerate() {
            self.insert_message(session_id, idx as i64, msg, now)
                .await?;
        }
        Ok(())
    }

    pub async fn append_messages(&self, session_id: &str, messages: &[Message]) -> Result<()> {
        let now = Utc::now().timestamp();
        self.upsert_session_metadata(session_id, now, None, None)
            .await?;
        // Scope the SELECT so its row stream is fully dropped before the
        // INSERTs below — an open statement on the same turso connection
        // can otherwise swallow subsequent writes.
        let next_pos: i64 = {
            let mut rows = self
                .conn
                .query(
                    "SELECT COALESCE(MAX(position), -1) + 1 FROM messages WHERE session_id = ?1",
                    (session_id.to_string(),),
                )
                .await?;
            match rows.next().await? {
                Some(row) => row.get(0)?,
                None => 0,
            }
        };
        for (offset, msg) in messages.iter().enumerate() {
            self.insert_message(session_id, next_pos + offset as i64, msg, now)
                .await?;
        }
        Ok(())
    }

    pub async fn save_provider_profile(
        &self,
        session_id: &str,
        provider_profile: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().timestamp();
        self.conn
            .execute(
                "INSERT INTO sessions (id, created_at, last_active, provider_profile) \
                 VALUES (?1, ?2, ?2, ?3) \
                 ON CONFLICT(id) DO UPDATE SET \
                     last_active = excluded.last_active, \
                     provider_profile = excluded.provider_profile",
                turso::params_from_iter([
                    turso::Value::from(session_id.to_string()),
                    turso::Value::from(now),
                    provider_profile.map(str::to_string).into(),
                ]),
            )
            .await?;
        Ok(())
    }

    pub async fn load_provider_profile(&self, session_id: &str) -> Result<Option<String>> {
        let mut rows = self
            .conn
            .query(
                "SELECT provider_profile FROM sessions WHERE id = ?1",
                (session_id.to_string(),),
            )
            .await?;
        match rows.next().await? {
            Some(row) => Ok(row.get(0)?),
            None => Ok(None),
        }
    }

    pub async fn save_session_metadata(
        &self,
        session_id: &str,
        parent_session_id: Option<&str>,
        lineage_label: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().timestamp();
        self.upsert_session_metadata(session_id, now, parent_session_id, lineage_label)
            .await
    }

    pub async fn list_sessions(&self, limit: usize) -> Result<Vec<SessionSummary>> {
        let mut rows = self
            .conn
            .query(
                "SELECT s.id, s.created_at, s.last_active, s.parent_session_id, s.lineage_label, \
                        s.provider_profile, \
                        (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id) AS msg_count, \
                        (SELECT content FROM messages m WHERE m.session_id = s.id AND m.role = 'user' \
                         ORDER BY m.position ASC LIMIT 1) AS preview \
                 FROM sessions s ORDER BY s.last_active DESC LIMIT ?1",
                (limit as i64,),
            )
            .await?;
        let mut summaries = Vec::new();
        while let Some(row) = rows.next().await? {
            let msg_count: i64 = row.get(6)?;
            let preview: Option<String> = row.get(7)?;
            summaries.push(SessionSummary {
                id: row.get(0)?,
                created_at: row.get(1)?,
                last_active: row.get(2)?,
                parent_session_id: row.get(3)?,
                lineage_label: row.get(4)?,
                provider_profile: row.get(5)?,
                message_count: msg_count as usize,
                preview: preview.map(|s| s.chars().take(60).collect::<String>().replace('\n', " ")),
            });
        }
        Ok(summaries)
    }

    pub async fn search_messages(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        // Negate fts_score so the returned score keeps FTS5's
        // "lower = better" contract (see module docs).
        let mut rows = self
            .conn
            .query(
                "SELECT session_id, position, role, content, created_at, \
                        -fts_score(content, ?1) AS score \
                 FROM messages WHERE fts_match(content, ?1) \
                 ORDER BY score ASC LIMIT ?2",
                (query.to_string(), limit as i64),
            )
            .await?;
        let mut hits = Vec::new();
        while let Some(row) = rows.next().await? {
            let content: Option<String> = row.get(3)?;
            hits.push(SearchHit {
                session_id: row.get(0)?,
                position: row.get(1)?,
                role: row.get(2)?,
                content: content.unwrap_or_default(),
                created_at: row.get(4)?,
                score: row.get(5)?,
            });
        }
        Ok(hits)
    }
}

// GH #704 parity tests: the store must satisfy the full SessionStore
// contract, including the FTS search behavior RecallHook and
// session.search depend on.
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trips_messages_and_lists_sessions() {
        let store = SessionStore::in_memory().await;
        let id = uuid::Uuid::new_v4().to_string();
        let messages = vec![
            Message::System {
                content: "you are a helpful agent".into(),
            },
            Message::User {
                content: "what is rust?".into(),
            },
            Message::Assistant {
                content: Some("a systems language".into()),
                tool_calls: None,
                reasoning_content: None,
            },
        ];
        store.save_messages(&id, &messages).await.unwrap();
        let loaded = store.load_history(&id).await.unwrap().unwrap();
        assert_eq!(loaded.len(), 3);

        store
            .append_messages(
                &id,
                &[Message::User {
                    content: "follow-up".into(),
                }],
            )
            .await
            .unwrap();
        assert_eq!(store.load_history(&id).await.unwrap().unwrap().len(), 4);

        let summaries = store.list_sessions(10).await.unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].message_count, 4);
        assert_eq!(store.last_session_id().await.as_deref(), Some(id.as_str()));
    }

    #[tokio::test]
    async fn provider_profile_and_lineage_round_trip() {
        let store = SessionStore::in_memory().await;
        let id = uuid::Uuid::new_v4().to_string();
        assert_eq!(store.load_provider_profile(&id).await.unwrap(), None);
        store
            .save_provider_profile(&id, Some("local"))
            .await
            .unwrap();
        assert_eq!(
            store.load_provider_profile(&id).await.unwrap().as_deref(),
            Some("local")
        );
        let child = uuid::Uuid::new_v4().to_string();
        store
            .save_session_metadata(&child, Some(&id), Some("forked"))
            .await
            .unwrap();
        let summaries = store.list_sessions(10).await.unwrap();
        let c = summaries.iter().find(|s| s.id == child).expect("child");
        assert_eq!(c.parent_session_id.as_deref(), Some(id.as_str()));
    }

    // The FTS thesis: native Turso FTS must find content, rank by
    // relevance, honor explicit-AND multi-token queries (the shape
    // sanitize_fts_query emits), and keep the negated "lower = better"
    // score contract RecallHook filters on.
    #[tokio::test]
    async fn fts_search_finds_ranks_and_keeps_score_contract() {
        let store = SessionStore::in_memory().await;
        let id = uuid::Uuid::new_v4().to_string();
        store
            .save_messages(
                &id,
                &[
                    Message::User {
                        content: "the quick brown fox jumps over the lazy dog".into(),
                    },
                    Message::User {
                        content: "lorem ipsum dolor sit amet".into(),
                    },
                    Message::User {
                        content: "the run_prompt_direct function streams tokens".into(),
                    },
                ],
            )
            .await
            .unwrap();

        let hits = store.search_messages("brown AND fox", 10).await.unwrap();
        assert!(
            hits.iter().any(|h| h.content.contains("brown fox")),
            "expected fox hit, got {hits:?}"
        );
        assert!(
            hits.iter().all(|h| h.score <= 0.0),
            "scores must be negated to keep the lower-is-better contract, got {hits:?}"
        );

        // Prefix query matches a code identifier.
        let hits = store.search_messages("run*", 10).await.unwrap();
        assert!(hits.iter().any(|h| h.content.contains("run_prompt_direct")));

        // Updates keep the index in sync without triggers: rewriting
        // the session must drop the old rows from search results.
        store
            .save_messages(
                &id,
                &[Message::User {
                    content: "completely different now".into(),
                }],
            )
            .await
            .unwrap();
        let hits = store.search_messages("brown AND fox", 10).await.unwrap();
        assert!(
            hits.is_empty(),
            "stale rows must leave the index after rewrite, got {hits:?}"
        );
    }
}
