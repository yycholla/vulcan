//! YYC-231 (YYC-166 PR-3): persistent install state for
//! discovered extensions.
//!
//! Lives at `~/.vulcan/extension_state.db` in the rusqlite backend.
//! The Turso migration uses `extension_install.turso.db` so feature
//! flips cannot one-way-convert the legacy DB. One row per installed
//! extension id, persisted across restarts. Provides the enable/disable
//! flag the registry consults on activation plus the last load-error
//! message for `vulcan extension list`.

use anyhow::{Context, Result};
#[cfg(not(feature = "turso-backend"))]
use parking_lot::Mutex;
#[cfg(not(feature = "turso-backend"))]
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
#[cfg(feature = "turso-backend")]
use std::sync::Arc;

use super::policy::{ExtensionPermission, PolicyDecision};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeAuditRecord {
    pub extension_id: String,
    pub requested_permission: Option<ExtensionPermission>,
    pub decision: PolicyDecision,
    pub allowed: bool,
    pub failure_reason: Option<String>,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
}

pub trait InstallStateStore: Send + Sync {
    fn upsert(&self, state: &InstallState) -> Result<()>;
    fn get(&self, id: &str) -> Result<Option<InstallState>>;
    fn list(&self) -> Result<Vec<InstallState>>;
    fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool>;
    fn record_load_error(&self, id: &str, error: &str) -> Result<bool>;
    fn clear_load_error(&self, id: &str) -> Result<bool>;
    fn record_runtime_audit(&self, record: &RuntimeAuditRecord) -> Result<()>;
    fn list_runtime_audit(&self, id: &str, limit: usize) -> Result<Vec<RuntimeAuditRecord>>;
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
    #[cfg(not(feature = "turso-backend"))]
    conn: Mutex<Connection>,
    #[cfg(feature = "turso-backend")]
    conn: Arc<turso::Connection>,
}

impl SqliteInstallStateStore {
    pub fn try_new() -> Result<Self> {
        let dir = crate::config::vulcan_home();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create vulcan_home at {}", dir.display()))?;
        Self::try_open_at(&dir.join(Self::db_file_name()))
    }

    #[cfg(not(feature = "turso-backend"))]
    fn db_file_name() -> &'static str {
        "extension_state.db"
    }

    #[cfg(feature = "turso-backend")]
    fn db_file_name() -> &'static str {
        "extension_install.turso.db"
    }

    #[cfg(not(feature = "turso-backend"))]
    pub fn try_open_at(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("open extension state DB at {}", path.display()))?;
        Self::initialize(&conn)
            .with_context(|| format!("init extension state schema at {}", path.display()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    #[cfg(feature = "turso-backend")]
    pub fn try_open_at(path: &Path) -> Result<Self> {
        let conn = crate::db::block_on(crate::db::open(path))?;
        crate::db::block_on(Self::initialize(&conn))
            .with_context(|| format!("init extension install schema at {}", path.display()))?;
        Ok(Self {
            conn: Arc::new(conn),
        })
    }

    #[cfg(not(feature = "turso-backend"))]
    pub fn try_open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory extension state DB")?;
        Self::initialize(&conn).context("init in-memory extension state schema")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    #[cfg(feature = "turso-backend")]
    pub fn try_open_in_memory() -> Result<Self> {
        let conn = crate::db::block_on(crate::db::open_in_memory())
            .context("open in-memory extension install DB")?;
        crate::db::block_on(Self::initialize(&conn))
            .context("init in-memory extension install schema")?;
        Ok(Self {
            conn: Arc::new(conn),
        })
    }

    #[cfg(not(feature = "turso-backend"))]
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

            CREATE TABLE IF NOT EXISTS extension_runtime_audit (
                id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                extension_id         TEXT NOT NULL,
                requested_permission TEXT,
                decision_json        TEXT NOT NULL,
                allowed              INTEGER NOT NULL,
                failure_reason       TEXT,
                occurred_at          TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_extension_runtime_audit_ext_time
                ON extension_runtime_audit(extension_id, occurred_at DESC);
            "#,
        )?;
        Ok(())
    }

    #[cfg(feature = "turso-backend")]
    async fn initialize(conn: &turso::Connection) -> Result<()> {
        for stmt in [
            "CREATE TABLE IF NOT EXISTS install_state (
                id              TEXT PRIMARY KEY,
                version         TEXT NOT NULL,
                enabled         INTEGER NOT NULL,
                installed_at    TEXT NOT NULL,
                last_load_error TEXT
            )",
            "CREATE TABLE IF NOT EXISTS extension_trust (
                workspace_hash    TEXT NOT NULL,
                extension_id      TEXT NOT NULL,
                manifest_checksum TEXT NOT NULL,
                trusted_at        TEXT NOT NULL,
                PRIMARY KEY (workspace_hash, extension_id)
            )",
            "CREATE TABLE IF NOT EXISTS extension_runtime_audit (
                id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                extension_id         TEXT NOT NULL,
                requested_permission TEXT,
                decision_json        TEXT NOT NULL,
                allowed              INTEGER NOT NULL,
                failure_reason       TEXT,
                occurred_at          TEXT NOT NULL
            )",
            "CREATE INDEX IF NOT EXISTS idx_extension_runtime_audit_ext_time
                ON extension_runtime_audit(extension_id, occurred_at DESC)",
        ] {
            crate::db::execute_ddl(conn, stmt).await?;
        }
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

#[cfg(not(feature = "turso-backend"))]
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

#[cfg(not(feature = "turso-backend"))]
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

    fn record_runtime_audit(&self, record: &RuntimeAuditRecord) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO extension_runtime_audit
                (extension_id, requested_permission, decision_json, allowed, failure_reason, occurred_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                record.extension_id,
                record.requested_permission.map(|p| p.as_str().to_string()),
                serde_json::to_string(&record.decision)?,
                record.allowed as i64,
                record.failure_reason,
                record.occurred_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    fn list_runtime_audit(&self, id: &str, limit: usize) -> Result<Vec<RuntimeAuditRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT extension_id, requested_permission, decision_json, allowed, failure_reason, occurred_at
             FROM extension_runtime_audit
             WHERE extension_id = ?1
             ORDER BY occurred_at DESC, rowid DESC
             LIMIT ?2",
        )?;
        let rows: Vec<_> = stmt
            .query_map(params![id, limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        rows.into_iter()
            .map(
                |(
                    extension_id,
                    permission,
                    decision_json,
                    allowed,
                    failure_reason,
                    occurred_at,
                )| {
                    let requested_permission =
                        permission.as_deref().and_then(ExtensionPermission::parse);
                    Ok(RuntimeAuditRecord {
                        extension_id,
                        requested_permission,
                        decision: serde_json::from_str(&decision_json)?,
                        allowed: allowed != 0,
                        failure_reason,
                        occurred_at: chrono::DateTime::parse_from_rfc3339(&occurred_at)?
                            .with_timezone(&chrono::Utc),
                    })
                },
            )
            .collect()
    }

    fn remove(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute("DELETE FROM install_state WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }
}

#[cfg(feature = "turso-backend")]
impl ExtensionTrustStore for SqliteInstallStateStore {
    fn trust(
        &self,
        workspace_hash: &str,
        extension_id: &str,
        manifest_checksum: &str,
    ) -> Result<()> {
        let workspace_hash = workspace_hash.to_string();
        let extension_id = extension_id.to_string();
        let manifest_checksum = manifest_checksum.to_string();
        let trusted_at = chrono::Utc::now().to_rfc3339();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            conn.execute(
                "INSERT INTO extension_trust
                    (workspace_hash, extension_id, manifest_checksum, trusted_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(workspace_hash, extension_id) DO UPDATE SET
                    manifest_checksum = excluded.manifest_checksum,
                    trusted_at = excluded.trusted_at",
                (workspace_hash, extension_id, manifest_checksum, trusted_at),
            )
            .await?;
            Ok(())
        })
    }

    fn untrust(&self, workspace_hash: &str, extension_id: &str) -> Result<bool> {
        let workspace_hash = workspace_hash.to_string();
        let extension_id = extension_id.to_string();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            let n = conn
                .execute(
                    "DELETE FROM extension_trust WHERE workspace_hash = ?1 AND extension_id = ?2",
                    (workspace_hash, extension_id),
                )
                .await?;
            Ok(n > 0)
        })
    }

    fn is_trusted(
        &self,
        workspace_hash: &str,
        extension_id: &str,
        manifest_checksum: &str,
    ) -> Result<bool> {
        let workspace_hash = workspace_hash.to_string();
        let extension_id = extension_id.to_string();
        let manifest_checksum = manifest_checksum.to_string();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            let mut rows = conn
                .query(
                    "SELECT manifest_checksum FROM extension_trust
                     WHERE workspace_hash = ?1 AND extension_id = ?2",
                    (workspace_hash, extension_id),
                )
                .await?;
            let Some(row) = rows.next().await? else {
                return Ok(false);
            };
            let found: String = row.get(0)?;
            Ok(found == manifest_checksum)
        })
    }

    fn list_trusted(&self, workspace_hash: &str) -> Result<Vec<TrustMarker>> {
        let workspace_hash = workspace_hash.to_string();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            let mut rows = conn
                .query(
                    "SELECT workspace_hash, extension_id, manifest_checksum, trusted_at
                     FROM extension_trust WHERE workspace_hash = ?1 ORDER BY extension_id ASC",
                    (workspace_hash,),
                )
                .await?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await? {
                let workspace_hash: String = row.get(0)?;
                let extension_id: String = row.get(1)?;
                let manifest_checksum: String = row.get(2)?;
                let trusted_at: String = row.get(3)?;
                out.push(TrustMarker {
                    workspace_hash,
                    extension_id,
                    manifest_checksum,
                    trusted_at: chrono::DateTime::parse_from_rfc3339(&trusted_at)?
                        .with_timezone(&chrono::Utc),
                });
            }
            Ok(out)
        })
    }
}

#[cfg(feature = "turso-backend")]
impl InstallStateStore for SqliteInstallStateStore {
    fn upsert(&self, state: &InstallState) -> Result<()> {
        let id = state.id.clone();
        let version = state.version.clone();
        let enabled = state.enabled as i64;
        let installed_at = state.installed_at.to_rfc3339();
        let last_load_error = state.last_load_error.clone();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            conn.execute(
                "INSERT INTO install_state (id, version, enabled, installed_at, last_load_error)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET
                    version = excluded.version,
                    enabled = excluded.enabled,
                    installed_at = excluded.installed_at,
                    last_load_error = excluded.last_load_error",
                turso::params_from_iter([
                    turso::Value::from(id),
                    turso::Value::from(version),
                    turso::Value::from(enabled),
                    turso::Value::from(installed_at),
                    last_load_error.into(),
                ]),
            )
            .await?;
            Ok(())
        })
    }

    fn get(&self, id: &str) -> Result<Option<InstallState>> {
        let id = id.to_string();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            let mut rows = conn
                .query(
                    "SELECT id, version, enabled, installed_at, last_load_error
                     FROM install_state WHERE id = ?1",
                    (id,),
                )
                .await?;
            let Some(row) = rows.next().await? else {
                return Ok(None);
            };
            Ok(Some(Self::row_to_state(
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            )?))
        })
    }

    fn list(&self) -> Result<Vec<InstallState>> {
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            let mut rows = conn
                .query(
                    "SELECT id, version, enabled, installed_at, last_load_error
                     FROM install_state ORDER BY id ASC",
                    (),
                )
                .await?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await? {
                out.push(Self::row_to_state(
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                )?);
            }
            Ok(out)
        })
    }

    fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool> {
        let id = id.to_string();
        let enabled = enabled as i64;
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            let n = conn
                .execute(
                    "UPDATE install_state SET enabled = ?1 WHERE id = ?2",
                    (enabled, id),
                )
                .await?;
            Ok(n > 0)
        })
    }

    fn record_load_error(&self, id: &str, error: &str) -> Result<bool> {
        let id = id.to_string();
        let error = error.to_string();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            let n = conn
                .execute(
                    "UPDATE install_state SET last_load_error = ?1 WHERE id = ?2",
                    (error, id),
                )
                .await?;
            Ok(n > 0)
        })
    }

    fn clear_load_error(&self, id: &str) -> Result<bool> {
        let id = id.to_string();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            let n = conn
                .execute(
                    "UPDATE install_state SET last_load_error = NULL WHERE id = ?1",
                    (id,),
                )
                .await?;
            Ok(n > 0)
        })
    }

    fn record_runtime_audit(&self, record: &RuntimeAuditRecord) -> Result<()> {
        let extension_id = record.extension_id.clone();
        let requested_permission = record.requested_permission.map(|p| p.as_str().to_string());
        let decision_json = serde_json::to_string(&record.decision)?;
        let allowed = record.allowed as i64;
        let failure_reason = record.failure_reason.clone();
        let occurred_at = record.occurred_at.to_rfc3339();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            conn.execute(
                "INSERT INTO extension_runtime_audit
                    (extension_id, requested_permission, decision_json, allowed, failure_reason, occurred_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                turso::params_from_iter([
                    turso::Value::from(extension_id),
                    requested_permission.into(),
                    turso::Value::from(decision_json),
                    turso::Value::from(allowed),
                    failure_reason.into(),
                    turso::Value::from(occurred_at),
                ]),
            )
            .await?;
            Ok(())
        })
    }

    fn list_runtime_audit(&self, id: &str, limit: usize) -> Result<Vec<RuntimeAuditRecord>> {
        let id = id.to_string();
        let limit = limit as i64;
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            let mut rows = conn
                .query(
                    "SELECT extension_id, requested_permission, decision_json, allowed, failure_reason, occurred_at
                     FROM extension_runtime_audit
                     WHERE extension_id = ?1
                     ORDER BY occurred_at DESC, rowid DESC
                     LIMIT ?2",
                    (id, limit),
                )
                .await?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().await? {
                let extension_id: String = row.get(0)?;
                let permission: Option<String> = row.get(1)?;
                let decision_json: String = row.get(2)?;
                let allowed: i64 = row.get(3)?;
                let failure_reason: Option<String> = row.get(4)?;
                let occurred_at: String = row.get(5)?;
                out.push(RuntimeAuditRecord {
                    extension_id,
                    requested_permission: permission
                        .as_deref()
                        .and_then(ExtensionPermission::parse),
                    decision: serde_json::from_str(&decision_json)?,
                    allowed: allowed != 0,
                    failure_reason,
                    occurred_at: chrono::DateTime::parse_from_rfc3339(&occurred_at)?
                        .with_timezone(&chrono::Utc),
                });
            }
            Ok(out)
        })
    }

    fn remove(&self, id: &str) -> Result<bool> {
        let id = id.to_string();
        let conn = Arc::clone(&self.conn);
        crate::db::block_on(async move {
            let n = conn
                .execute("DELETE FROM install_state WHERE id = ?1", (id,))
                .await?;
            Ok(n > 0)
        })
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
        assert!(!store.get(&s.id).unwrap().unwrap().enabled);
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
    fn runtime_audit_records_policy_decisions() {
        let store = SqliteInstallStateStore::try_open_in_memory().unwrap();
        let record = RuntimeAuditRecord {
            extension_id: "tooler".into(),
            requested_permission: Some(ExtensionPermission::ToolRegistration),
            decision: PolicyDecision::Deny {
                reason: "missing declaration".into(),
            },
            allowed: false,
            failure_reason: Some("missing declaration".into()),
            occurred_at: chrono::Utc::now(),
        };
        store.record_runtime_audit(&record).unwrap();
        let listed = store.list_runtime_audit("tooler", 10).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].requested_permission,
            Some(ExtensionPermission::ToolRegistration)
        );
        assert!(!listed[0].allowed);
        assert!(matches!(listed[0].decision, PolicyDecision::Deny { .. }));
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

    #[cfg(feature = "turso-backend")]
    #[test]
    fn turso_backend_uses_isolated_file_name() {
        assert_eq!(
            SqliteInstallStateStore::db_file_name(),
            "extension_install.turso.db"
        );
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
