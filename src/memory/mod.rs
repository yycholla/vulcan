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

mod codec;
pub mod cortex;
mod turso_store;

/// Persistent session storage.
///
/// CRUD methods are async and run on Turso's native connection type.
pub struct SessionStore {
    pub(in crate::memory) conn: turso::Connection,
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
