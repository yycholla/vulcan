//! YYC-231 (YYC-166 PR-3): persistent install state for
//! discovered extensions.
//!
//! Lives at `~/.vulcan/extension_state.db`. One row per
//! installed extension id, persisted across restarts. Provides
//! the enable/disable flag the registry consults on activation
//! plus the last load-error message for `vulcan extension list`.

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallState {
    pub id: String,
    pub version: String,
    pub enabled: bool,
    pub installed_at: chrono::DateTime<chrono::Utc>,
    pub last_load_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustMarker {
    pub workspace_hash: String,
    pub extension_id: String,
    pub manifest_checksum: String,
    pub trusted_at: chrono::DateTime<chrono::Utc>,
}

pub trait InstallStateStore: Send + Sync {
    fn upsert(&self, state: &InstallState) -> Result<()>;
    fn get(&self, id: &str) -> Result<Option<InstallState>>;
    fn list(&self) -> Result<Vec<InstallState>>;
    fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool>;
    fn record_load_error(&self, id: &str, error: &str) -> Result<bool>;
    fn clear_load_error(&self, id: &str) -> Result<bool>;
    fn remove(&self, id: &str) -> Result<bool>;
}

pub trait ExtensionTrustStore: Send + Sync {
    fn trust(
        &self,
        workspace_hash: &str,
        extension_id: &str,
        manifest_checksum: &str,
    ) -> Result<()>;
    fn untrust(&self, workspace_hash: &str, extension_id: &str) -> Result<bool>;
    fn is_trusted(
        &self,
        workspace_hash: &str,
        extension_id: &str,
        manifest_checksum: &str,
    ) -> Result<bool>;
    fn list_trusted(&self, workspace_hash: &str) -> Result<Vec<TrustMarker>>;
}

pub struct SqliteInstallStateStore {
    conn: Mutex<Connection>,
}

impl SqliteInstallStateStore {
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
            conn: Mutex::new(conn),
        })
    }

    pub fn try_open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory extension state DB")?;
        Self::initialize(&conn).context("init in-memory extension state schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn initialize(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS install_state (
                id              TEXT PRIMARY KEY,
                version         TEXT NOT NULL,
                enabled         INTEGER NOT NULL,
                installed_at    TEXT NOT NULL,
                last_load_error TEXT
            );

            CREATE TABLE IF NOT EXISTS extension_trust (
                workspace_hash    TEXT NOT NULL,
                extension_id      TEXT NOT NULL,
                manifest_checksum TEXT NOT NULL,
                trusted_at        TEXT NOT NULL,
                PRIMARY KEY (workspace_hash, extension_id)
            );
            "#,
        )?;
        Ok(())
    }

    fn row_to_state(
        id: String,
        version: String,
        enabled: i64,
        installed_at: String,
        last_load_error: Option<String>,
    ) -> Result<InstallState> {
        Ok(InstallState {
            id,
            version,
            enabled: enabled != 0,
            installed_at: chrono::DateTime::parse_from_rfc3339(&installed_at)?
                .with_timezone(&chrono::Utc),
            last_load_error,
        })
    }
}

impl ExtensionTrustStore for SqliteInstallStateStore {
    fn trust(
        &self,
        workspace_hash: &str,
        extension_id: &str,
        manifest_checksum: &str,
    ) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO extension_trust
                (workspace_hash, extension_id, manifest_checksum, trusted_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(workspace_hash, extension_id) DO UPDATE SET
                manifest_checksum = excluded.manifest_checksum,
                trusted_at = excluded.trusted_at",
            params![
                workspace_hash,
                extension_id,
                manifest_checksum,
                chrono::Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    fn untrust(&self, workspace_hash: &str, extension_id: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "DELETE FROM extension_trust WHERE workspace_hash = ?1 AND extension_id = ?2",
            params![workspace_hash, extension_id],
        )?;
        Ok(n > 0)
    }

    fn is_trusted(
        &self,
        workspace_hash: &str,
        extension_id: &str,
        manifest_checksum: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock();
        let found: Option<String> = conn
            .query_row(
                "SELECT manifest_checksum FROM extension_trust
                 WHERE workspace_hash = ?1 AND extension_id = ?2",
                params![workspace_hash, extension_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.as_deref() == Some(manifest_checksum))
    }

    fn list_trusted(&self, workspace_hash: &str) -> Result<Vec<TrustMarker>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT workspace_hash, extension_id, manifest_checksum, trusted_at
             FROM extension_trust WHERE workspace_hash = ?1 ORDER BY extension_id ASC",
        )?;
        let rows: Vec<_> = stmt
            .query_map(params![workspace_hash], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        rows.into_iter()
            .map(
                |(workspace_hash, extension_id, manifest_checksum, trusted_at)| {
                    Ok(TrustMarker {
                        workspace_hash,
                        extension_id,
                        manifest_checksum,
                        trusted_at: chrono::DateTime::parse_from_rfc3339(&trusted_at)?
                            .with_timezone(&chrono::Utc),
                    })
                },
            )
            .collect()
    }
}

impl InstallStateStore for SqliteInstallStateStore {
    fn upsert(&self, state: &InstallState) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO install_state (id, version, enabled, installed_at, last_load_error)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                version = excluded.version,
                enabled = excluded.enabled,
                installed_at = excluded.installed_at,
                last_load_error = excluded.last_load_error",
            params![
                state.id,
                state.version,
                state.enabled as i64,
                state.installed_at.to_rfc3339(),
                state.last_load_error,
            ],
        )?;
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Option<InstallState>> {
        let conn = self.conn.lock();
        let row = conn
            .query_row(
                "SELECT id, version, enabled, installed_at, last_load_error
                 FROM install_state WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;
        match row {
            Some(t) => Ok(Some(Self::row_to_state(t.0, t.1, t.2, t.3, t.4)?)),
            None => Ok(None),
        }
    }

    fn list(&self) -> Result<Vec<InstallState>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, version, enabled, installed_at, last_load_error
             FROM install_state ORDER BY id ASC",
        )?;
        let rows: Vec<_> = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        rows.into_iter()
            .map(|t| Self::row_to_state(t.0, t.1, t.2, t.3, t.4))
            .collect()
    }

    fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE install_state SET enabled = ?1 WHERE id = ?2",
            params![enabled as i64, id],
        )?;
        Ok(n > 0)
    }

    fn record_load_error(&self, id: &str, error: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE install_state SET last_load_error = ?1 WHERE id = ?2",
            params![error, id],
        )?;
        Ok(n > 0)
    }

    fn clear_load_error(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE install_state SET last_load_error = NULL WHERE id = ?1",
            params![id],
        )?;
        Ok(n > 0)
    }

    fn remove(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute("DELETE FROM install_state WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fixture() -> InstallState {
        InstallState {
            id: "lint-helper".into(),
            version: "0.1.0".into(),
            enabled: true,
            installed_at: chrono::Utc::now(),
            last_load_error: None,
        }
    }

    #[test]
    fn upsert_and_get_round_trip() {
        let store = SqliteInstallStateStore::try_open_in_memory().unwrap();
        let s = fixture();
        store.upsert(&s).unwrap();
        let got = store.get(&s.id).unwrap().unwrap();
        assert_eq!(got.id, s.id);
        assert_eq!(got.version, s.version);
        assert_eq!(got.enabled, s.enabled);
    }

    #[test]
    fn list_returns_ids_in_alphabetical_order() {
        let store = SqliteInstallStateStore::try_open_in_memory().unwrap();
        let mut a = fixture();
        a.id = "alpha".into();
        let mut z = fixture();
        z.id = "zulu".into();
        let mut m = fixture();
        m.id = "mike".into();
        store.upsert(&z).unwrap();
        store.upsert(&a).unwrap();
        store.upsert(&m).unwrap();
        let ids: Vec<String> = store.list().unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["alpha", "mike", "zulu"]);
    }

    #[test]
    fn set_enabled_returns_false_for_missing_id() {
        let store = SqliteInstallStateStore::try_open_in_memory().unwrap();
        assert!(!store.set_enabled("ghost", false).unwrap());
    }

    #[test]
    fn set_enabled_persists_change() {
        let store = SqliteInstallStateStore::try_open_in_memory().unwrap();
        let s = fixture();
        store.upsert(&s).unwrap();
        assert!(store.set_enabled(&s.id, false).unwrap());
        assert_eq!(store.get(&s.id).unwrap().unwrap().enabled, false);
    }

    #[test]
    fn record_and_clear_load_error_round_trip() {
        let store = SqliteInstallStateStore::try_open_in_memory().unwrap();
        let s = fixture();
        store.upsert(&s).unwrap();
        assert!(store.record_load_error(&s.id, "missing entry").unwrap());
        assert_eq!(
            store
                .get(&s.id)
                .unwrap()
                .unwrap()
                .last_load_error
                .as_deref(),
            Some("missing entry")
        );
        assert!(store.clear_load_error(&s.id).unwrap());
        assert!(store.get(&s.id).unwrap().unwrap().last_load_error.is_none());
    }

    #[test]
    fn remove_returns_false_when_id_missing() {
        let store = SqliteInstallStateStore::try_open_in_memory().unwrap();
        assert!(!store.remove("ghost").unwrap());
    }

    #[test]
    fn state_survives_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let store = SqliteInstallStateStore::try_open_at(&path).unwrap();
            store.upsert(&fixture()).unwrap();
        }
        let reopened = SqliteInstallStateStore::try_open_at(&path).unwrap();
        let listed = reopened.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "lint-helper");
    }

    #[test]
    fn trust_marker_requires_current_checksum() {
        let store = SqliteInstallStateStore::try_open_in_memory().unwrap();
        store.trust("workspace-a", "todo", "sha256:old").unwrap();
        assert!(
            store
                .is_trusted("workspace-a", "todo", "sha256:old")
                .unwrap()
        );
        assert!(
            !store
                .is_trusted("workspace-a", "todo", "sha256:new")
                .unwrap()
        );

        store.trust("workspace-a", "todo", "sha256:new").unwrap();
        assert!(
            store
                .is_trusted("workspace-a", "todo", "sha256:new")
                .unwrap()
        );
        assert!(store.untrust("workspace-a", "todo").unwrap());
        assert!(
            !store
                .is_trusted("workspace-a", "todo", "sha256:new")
                .unwrap()
        );
    }
}
