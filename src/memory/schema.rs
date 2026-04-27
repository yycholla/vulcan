//! SQLite schema + idempotent migration helpers for the session store.
//!
//! Split out of `memory/mod.rs` so the (long) DDL string and migration
//! sequence don't share a file with the `SessionStore` CRUD surface
//! (YYC-111).

#[cfg(feature = "gateway")]
use anyhow::Context;
use anyhow::Result;
use rusqlite::{Connection, params};

/// Connection pool used by the gateway daemon (YYC-113). Replaces the
/// previous `Arc<Mutex<Connection>>` that serialized every gateway worker
/// through one lock. r2d2 hands each caller its own pooled connection;
/// SQLite handles concurrent readers + a single writer cleanly with WAL
/// mode (already set in `SCHEMA`).
#[cfg(feature = "gateway")]
pub type DbPool = r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>;

pub(in crate::memory) const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS sessions (
    id                TEXT PRIMARY KEY,
    created_at        INTEGER NOT NULL,
    last_active       INTEGER NOT NULL,
    parent_session_id TEXT,
    lineage_label     TEXT,
    provider_profile  TEXT
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

pub(in crate::memory) fn initialize_conn(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;

    // Idempotent migrations for DBs created before additive columns landed.
    let _ = conn.execute("ALTER TABLE messages ADD COLUMN reasoning_content TEXT", []);
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN parent_session_id TEXT", []);
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN lineage_label TEXT", []);
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN provider_profile TEXT", []);
    Ok(())
}

#[cfg(feature = "gateway")]
pub(crate) fn open_gateway_pool() -> Result<DbPool> {
    let dir = crate::config::vulcan_home();
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("sessions.db");
    let manager = r2d2_sqlite::SqliteConnectionManager::file(&path);
    let pool = r2d2::Pool::builder()
        .build(manager)
        .with_context(|| format!("Failed to build gateway DB pool at {}", path.display()))?;
    let conn = pool
        .get()
        .context("Failed to check out a connection for schema init")?;
    initialize_conn(&conn).context("Failed to initialize session DB schema")?;
    Ok(pool)
}

/// Build an in-memory pool for tests. Single connection so all checkouts
/// share state (a fresh `:memory:` per checkout would lose every prior
/// write). Consumers that need a multi-connection pool should use
/// `open_gateway_pool` against a temp file.
#[cfg(all(test, feature = "gateway"))]
pub(crate) fn in_memory_gateway_pool() -> Result<DbPool> {
    let manager = r2d2_sqlite::SqliteConnectionManager::memory();
    let pool = r2d2::Pool::builder()
        .max_size(1)
        .build(manager)
        .context("build in-memory gateway DB pool")?;
    let conn = pool.get().context("get conn from in-memory gateway pool")?;
    initialize_conn(&conn).context("initialize in-memory gateway DB schema")?;
    Ok(pool)
}

pub(in crate::memory) fn upsert_session_metadata(
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

/// Persist (or clear) the active named provider profile for a session
/// (YYC-95). `None` means the session uses the legacy unnamed `[provider]`
/// block; the row is created if it doesn't exist yet.
pub(in crate::memory) fn upsert_session_provider_profile(
    conn: &Connection,
    session_id: &str,
    now: i64,
    provider_profile: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions (id, created_at, last_active, provider_profile)
         VALUES (?1, ?2, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET
             last_active = excluded.last_active,
             provider_profile = excluded.provider_profile",
        params![session_id, now, provider_profile],
    )?;
    Ok(())
}
