//! Extension-scoped persistent state and checkpoints.
//!
//! This is the SQLite-first foundation for GH issue #270. Extensions receive
//! scoped handles, so normal key/value and checkpoint operations do not accept
//! an arbitrary extension id after construction.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
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
    conn: Arc<turso::Connection>,
}

impl SqliteExtensionStateStore {
    pub fn try_new() -> Result<Self> {
        let dir = crate::config::vulcan_home();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create vulcan_home at {}", dir.display()))?;
        Self::try_open_at(&dir.join(Self::db_file_name()))
    }

    fn db_file_name() -> &'static str {
        "extension_state.turso.db"
    }

    pub fn try_open_at(path: &Path) -> Result<Self> {
        let conn = crate::db::block_on(crate::db::open(path))?;
        crate::db::block_on(Self::initialize(&conn))
            .with_context(|| format!("init extension state schema at {}", path.display()))?;
        Ok(Self {
            conn: Arc::new(conn),
        })
    }

    pub fn try_open_in_memory() -> Result<Self> {
        let conn = crate::db::block_on(crate::db::open_in_memory())
            .context("open in-memory extension state DB")?;
        crate::db::block_on(Self::initialize(&conn))
            .context("init in-memory extension state schema")?;
        Ok(Self {
            conn: Arc::new(conn),
        })
    }

    async fn initialize(conn: &turso::Connection) -> Result<()> {
        for stmt in [
            "CREATE TABLE IF NOT EXISTS extension_state_kv (
                extension_id TEXT NOT NULL,
                key          TEXT NOT NULL,
                value_json   TEXT NOT NULL,
                updated_at   TEXT NOT NULL,
                PRIMARY KEY (extension_id, key)
            )",
            "CREATE TABLE IF NOT EXISTS extension_checkpoints (
                extension_id  TEXT NOT NULL,
                checkpoint_id TEXT NOT NULL,
                state_json    TEXT NOT NULL,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                PRIMARY KEY (extension_id, checkpoint_id)
            )",
            "CREATE INDEX IF NOT EXISTS idx_extension_checkpoints_updated
                ON extension_checkpoints(extension_id, updated_at DESC)",
        ] {
            crate::db::execute_ddl(conn, stmt).await?;
        }
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
    conn: Arc<turso::Connection>,
}

impl ExtensionStateScope {
    pub fn extension_id(&self) -> &str {
        &self.extension_id
    }

    pub fn put_json(&self, key: &str, value: &Value) -> Result<()> {
        validate_key(key)?;
        let extension_id = self.extension_id.clone();
        let key = key.to_string();
        let value_json = serde_json::to_string(value)?;
        let now = Utc::now().to_rfc3339();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            conn.execute(
                "INSERT INTO extension_state_kv (extension_id, key, value_json, updated_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(extension_id, key) DO UPDATE SET
                    value_json = excluded.value_json,
                    updated_at = excluded.updated_at",
                (extension_id, key, value_json, now),
            )
            .await?;
            Ok(())
        })
    }

    pub fn get_json(&self, key: &str) -> Result<Option<Value>> {
        validate_key(key)?;
        let extension_id = self.extension_id.clone();
        let key = key.to_string();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            let mut rows = conn
                .query(
                    "SELECT value_json FROM extension_state_kv
                     WHERE extension_id = ?1 AND key = ?2",
                    (extension_id, key),
                )
                .await?;
            let Some(row) = rows.next().await? else {
                return Ok(None);
            };
            let raw: String = row.get(0)?;
            Ok(Some(
                serde_json::from_str(&raw).context("decode extension state JSON")?,
            ))
        })
    }

    pub fn delete_key(&self, key: &str) -> Result<bool> {
        validate_key(key)?;
        let extension_id = self.extension_id.clone();
        let key = key.to_string();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            let n = conn
                .execute(
                    "DELETE FROM extension_state_kv WHERE extension_id = ?1 AND key = ?2",
                    (extension_id, key),
                )
                .await?;
            Ok(n > 0)
        })
    }

    pub fn list_keys(&self) -> Result<Vec<String>> {
        let extension_id = self.extension_id.clone();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            let mut rows = conn
                .query(
                    "SELECT key FROM extension_state_kv
                     WHERE extension_id = ?1 ORDER BY key ASC",
                    (extension_id,),
                )
                .await?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await? {
                out.push(row.get(0)?);
            }
            Ok(out)
        })
    }

    pub fn save_checkpoint(&self, checkpoint_id: &str, state: &Value) -> Result<()> {
        validate_identifier("checkpoint id", checkpoint_id)?;
        let extension_id = self.extension_id.clone();
        let checkpoint_id = checkpoint_id.to_string();
        let state_json = serde_json::to_string(state)?;
        let now = Utc::now().to_rfc3339();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            conn.execute(
                "INSERT INTO extension_checkpoints
                    (extension_id, checkpoint_id, state_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)
                 ON CONFLICT(extension_id, checkpoint_id) DO UPDATE SET
                    state_json = excluded.state_json,
                    updated_at = excluded.updated_at",
                (extension_id, checkpoint_id, state_json, now),
            )
            .await?;
            Ok(())
        })
    }

    pub fn restore_checkpoint(&self, checkpoint_id: &str) -> Result<Option<CheckpointRecord>> {
        validate_identifier("checkpoint id", checkpoint_id)?;
        let extension_id = self.extension_id.clone();
        let checkpoint_id = checkpoint_id.to_string();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            let mut rows = conn
                .query(
                    "SELECT checkpoint_id, state_json, created_at, updated_at
                     FROM extension_checkpoints
                     WHERE extension_id = ?1 AND checkpoint_id = ?2",
                    (extension_id, checkpoint_id),
                )
                .await?;
            let Some(row) = rows.next().await? else {
                return Ok(None);
            };
            let id: String = row.get(0)?;
            let state_json: String = row.get(1)?;
            let created_at: String = row.get(2)?;
            let updated_at: String = row.get(3)?;
            Ok(Some(CheckpointRecord {
                id,
                state: serde_json::from_str(&state_json).context("decode checkpoint JSON")?,
                created_at: parse_time(&created_at)?,
                updated_at: parse_time(&updated_at)?,
            }))
        })
    }

    pub fn list_checkpoints(&self) -> Result<Vec<CheckpointInfo>> {
        let extension_id = self.extension_id.clone();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            let mut rows = conn
                .query(
                    "SELECT checkpoint_id, created_at, updated_at
                     FROM extension_checkpoints
                     WHERE extension_id = ?1
                     ORDER BY updated_at DESC, checkpoint_id ASC",
                    (extension_id,),
                )
                .await?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await? {
                let id: String = row.get(0)?;
                let created_at: String = row.get(1)?;
                let updated_at: String = row.get(2)?;
                out.push(CheckpointInfo {
                    id,
                    created_at: parse_time(&created_at)?,
                    updated_at: parse_time(&updated_at)?,
                });
            }
            Ok(out)
        })
    }

    pub fn delete_checkpoint(&self, checkpoint_id: &str) -> Result<bool> {
        validate_identifier("checkpoint id", checkpoint_id)?;
        let extension_id = self.extension_id.clone();
        let checkpoint_id = checkpoint_id.to_string();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            let n = conn
                .execute(
                    "DELETE FROM extension_checkpoints
                     WHERE extension_id = ?1 AND checkpoint_id = ?2",
                    (extension_id, checkpoint_id),
                )
                .await?;
            Ok(n > 0)
        })
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
    fn turso_backend_uses_isolated_file_name() {
        assert_eq!(
            SqliteExtensionStateStore::db_file_name(),
            "extension_state.turso.db"
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
    fn invalid_scope_ids_are_rejected() {
        let store = SqliteExtensionStateStore::try_open_in_memory().unwrap();
        assert!(store.scope("../bad").is_err());
    }
}
