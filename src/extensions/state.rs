//! Extension-scoped persistent state and checkpoints.
//!
//! This is the SQLite-first foundation for GH issue #270. Extensions receive
//! scoped handles, so normal key/value and checkpoint operations do not accept
//! an arbitrary extension id after construction.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointInfo {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointRecord {
    pub id: String,
    pub state: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub trait ExtensionStateStore: Send + Sync {
    fn scope(&self, extension_id: &str) -> Result<ExtensionStateScope>;
}

pub struct SqliteExtensionStateStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteExtensionStateStore {
    pub fn try_new() -> Result<Self> {
        let dir = crate::config::vulcan_home();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create vulcan_home at {}", dir.display()))?;
        Self::try_open_at(&dir.join("extension_state.db"))
    }

    pub fn try_open_at(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("open extension state DB at {}", path.display()))?;
        Self::initialize(&conn)
            .with_context(|| format!("init extension state schema at {}", path.display()))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn try_open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory extension state DB")?;
        Self::initialize(&conn).context("init in-memory extension state schema")?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn initialize(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS extension_state_kv (
                extension_id TEXT NOT NULL,
                key          TEXT NOT NULL,
                value_json   TEXT NOT NULL,
                updated_at   TEXT NOT NULL,
                PRIMARY KEY (extension_id, key)
            );

            CREATE TABLE IF NOT EXISTS extension_checkpoints (
                extension_id  TEXT NOT NULL,
                checkpoint_id TEXT NOT NULL,
                state_json    TEXT NOT NULL,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                PRIMARY KEY (extension_id, checkpoint_id)
            );

            CREATE INDEX IF NOT EXISTS idx_extension_checkpoints_updated
                ON extension_checkpoints(extension_id, updated_at DESC);
            "#,
        )?;
        Ok(())
    }
}

impl ExtensionStateStore for SqliteExtensionStateStore {
    fn scope(&self, extension_id: &str) -> Result<ExtensionStateScope> {
        validate_identifier("extension id", extension_id)?;
        Ok(ExtensionStateScope {
            extension_id: extension_id.to_string(),
            conn: Arc::clone(&self.conn),
        })
    }
}

#[derive(Clone)]
pub struct ExtensionStateScope {
    extension_id: String,
    conn: Arc<Mutex<Connection>>,
}

impl ExtensionStateScope {
    pub fn extension_id(&self) -> &str {
        &self.extension_id
    }

    pub fn put_json(&self, key: &str, value: &Value) -> Result<()> {
        validate_key(key)?;
        let value_json = serde_json::to_string(value)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO extension_state_kv (extension_id, key, value_json, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(extension_id, key) DO UPDATE SET
                value_json = excluded.value_json,
                updated_at = excluded.updated_at",
            params![self.extension_id, key, value_json, now],
        )?;
        Ok(())
    }

    pub fn get_json(&self, key: &str) -> Result<Option<Value>> {
        validate_key(key)?;
        let conn = self.conn.lock();
        let value_json: Option<String> = conn
            .query_row(
                "SELECT value_json FROM extension_state_kv
                 WHERE extension_id = ?1 AND key = ?2",
                params![self.extension_id, key],
                |row| row.get(0),
            )
            .optional()?;
        value_json
            .map(|raw| serde_json::from_str(&raw).context("decode extension state JSON"))
            .transpose()
    }

    pub fn delete_key(&self, key: &str) -> Result<bool> {
        validate_key(key)?;
        let conn = self.conn.lock();
        let n = conn.execute(
            "DELETE FROM extension_state_kv WHERE extension_id = ?1 AND key = ?2",
            params![self.extension_id, key],
        )?;
        Ok(n > 0)
    }

    pub fn list_keys(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT key FROM extension_state_kv
             WHERE extension_id = ?1 ORDER BY key ASC",
        )?;
        let rows = stmt
            .query_map(params![self.extension_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn save_checkpoint(&self, checkpoint_id: &str, state: &Value) -> Result<()> {
        validate_identifier("checkpoint id", checkpoint_id)?;
        let state_json = serde_json::to_string(state)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO extension_checkpoints
                (extension_id, checkpoint_id, state_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(extension_id, checkpoint_id) DO UPDATE SET
                state_json = excluded.state_json,
                updated_at = excluded.updated_at",
            params![self.extension_id, checkpoint_id, state_json, now],
        )?;
        Ok(())
    }

    pub fn restore_checkpoint(&self, checkpoint_id: &str) -> Result<Option<CheckpointRecord>> {
        validate_identifier("checkpoint id", checkpoint_id)?;
        let conn = self.conn.lock();
        let row = conn
            .query_row(
                "SELECT checkpoint_id, state_json, created_at, updated_at
                 FROM extension_checkpoints
                 WHERE extension_id = ?1 AND checkpoint_id = ?2",
                params![self.extension_id, checkpoint_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(id, state_json, created_at, updated_at)| {
            Ok(CheckpointRecord {
                id,
                state: serde_json::from_str(&state_json).context("decode checkpoint JSON")?,
                created_at: parse_time(&created_at)?,
                updated_at: parse_time(&updated_at)?,
            })
        })
        .transpose()
    }

    pub fn list_checkpoints(&self) -> Result<Vec<CheckpointInfo>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT checkpoint_id, created_at, updated_at
             FROM extension_checkpoints
             WHERE extension_id = ?1
             ORDER BY updated_at DESC, checkpoint_id ASC",
        )?;
        let rows = stmt
            .query_map(params![self.extension_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter()
            .map(|(id, created_at, updated_at)| {
                Ok(CheckpointInfo {
                    id,
                    created_at: parse_time(&created_at)?,
                    updated_at: parse_time(&updated_at)?,
                })
            })
            .collect()
    }

    pub fn delete_checkpoint(&self, checkpoint_id: &str) -> Result<bool> {
        validate_identifier("checkpoint id", checkpoint_id)?;
        let conn = self.conn.lock();
        let n = conn.execute(
            "DELETE FROM extension_checkpoints
             WHERE extension_id = ?1 AND checkpoint_id = ?2",
            params![self.extension_id, checkpoint_id],
        )?;
        Ok(n > 0)
    }
}

fn parse_time(raw: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(raw)?.with_timezone(&Utc))
}

fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() {
        bail!("extension state key must not be empty");
    }
    Ok(())
}

fn validate_identifier(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{label} must not be empty");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("{label} `{value}` contains unsupported characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn state_is_scoped_by_extension_id() {
        let store = SqliteExtensionStateStore::try_open_in_memory().unwrap();
        let alpha = store.scope("alpha").unwrap();
        let beta = store.scope("beta").unwrap();

        alpha
            .put_json("settings", &json!({"model": "fast"}))
            .unwrap();
        beta.put_json("settings", &json!({"model": "safe"}))
            .unwrap();

        assert_eq!(
            alpha.get_json("settings").unwrap(),
            Some(json!({"model": "fast"}))
        );
        assert_eq!(
            beta.get_json("settings").unwrap(),
            Some(json!({"model": "safe"}))
        );
        assert_eq!(alpha.list_keys().unwrap(), vec!["settings"]);
        assert_eq!(beta.list_keys().unwrap(), vec!["settings"]);
    }

    #[test]
    fn state_survives_reopening_file_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("extension_state.db");
        {
            let store = SqliteExtensionStateStore::try_open_at(&path).unwrap();
            let scoped = store.scope("persist-demo").unwrap();
            scoped.put_json("counter", &json!({"value": 3})).unwrap();
        }

        let reopened = SqliteExtensionStateStore::try_open_at(&path).unwrap();
        let scoped = reopened.scope("persist-demo").unwrap();
        assert_eq!(
            scoped.get_json("counter").unwrap(),
            Some(json!({"value": 3}))
        );
    }

    #[test]
    fn checkpoint_crud_is_scoped_and_round_trips_json() {
        let store = SqliteExtensionStateStore::try_open_in_memory().unwrap();
        let alpha = store.scope("alpha").unwrap();
        let beta = store.scope("beta").unwrap();

        alpha
            .save_checkpoint("draft", &json!({"step": 1, "items": ["a"]}))
            .unwrap();
        beta.save_checkpoint("draft", &json!({"step": 99})).unwrap();

        let restored = alpha.restore_checkpoint("draft").unwrap().unwrap();
        assert_eq!(restored.id, "draft");
        assert_eq!(restored.state, json!({"step": 1, "items": ["a"]}));
        assert_eq!(alpha.list_checkpoints().unwrap()[0].id, "draft");
        assert_eq!(
            beta.restore_checkpoint("draft").unwrap().unwrap().state,
            json!({"step": 99})
        );

        assert!(alpha.delete_checkpoint("draft").unwrap());
        assert!(alpha.restore_checkpoint("draft").unwrap().is_none());
        assert!(beta.restore_checkpoint("draft").unwrap().is_some());
    }

    #[test]
    fn migrations_are_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        SqliteExtensionStateStore::initialize(&conn).unwrap();
        SqliteExtensionStateStore::initialize(&conn).unwrap();

        conn.execute(
            "INSERT INTO extension_state_kv (extension_id, key, value_json, updated_at)
             VALUES ('alpha', 'k', '{}', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn invalid_scope_ids_are_rejected() {
        let store = SqliteExtensionStateStore::try_open_in_memory().unwrap();
        assert!(store.scope("../bad").is_err());
    }
}
