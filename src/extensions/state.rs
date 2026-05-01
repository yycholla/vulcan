use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::ExtensionCapability;
use crate::hooks::HookHandler;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BranchPolicy {
    #[default]
    Fork,
    InheritRef,
    Isolate,
}

impl BranchPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            BranchPolicy::Fork => "fork",
            BranchPolicy::InheritRef => "inherit_ref",
            BranchPolicy::Isolate => "isolate",
        }
    }
}

impl std::str::FromStr for BranchPolicy {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "fork" => Ok(Self::Fork),
            "inherit_ref" => Ok(Self::InheritRef),
            "isolate" => Ok(Self::Isolate),
            other => Err(format!("unknown branch policy `{other}`")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionStateEntry {
    pub session_id: String,
    pub extension_id: String,
    pub key: String,
    pub value: Value,
    pub branch_policy: BranchPolicy,
}

#[derive(Debug, Error)]
pub enum ExtensionStateError {
    #[error("extension `{extension_id}` is missing capability `cross_session_state`")]
    MissingCrossSessionCapability { extension_id: String },
    #[error(transparent)]
    Store(#[from] anyhow::Error),
}

pub trait ExtensionStateStore: Send + Sync {
    fn append_entry(
        &self,
        session_id: &str,
        extension_id: &str,
        key: &str,
        value: Value,
        branch_policy: BranchPolicy,
    ) -> Result<()>;

    fn get_entries(
        &self,
        session_id: &str,
        extension_id: &str,
        prefix: &str,
    ) -> Result<Vec<ExtensionStateEntry>>;

    fn branch_session(
        &self,
        parent_session_id: &str,
        child_session_id: &str,
        active_extension_ids: &[String],
    ) -> Result<usize>;

    fn reap_session(&self, session_id: &str) -> Result<usize>;
}

#[derive(Clone)]
pub struct ExtensionStateContext {
    store: Arc<dyn ExtensionStateStore>,
    session_id: String,
    extension_id: String,
    capabilities: Vec<ExtensionCapability>,
}

impl ExtensionStateContext {
    pub fn new(
        store: Arc<dyn ExtensionStateStore>,
        session_id: impl Into<String>,
        extension_id: impl Into<String>,
        capabilities: Vec<ExtensionCapability>,
    ) -> Self {
        Self {
            store,
            session_id: session_id.into(),
            extension_id: extension_id.into(),
            capabilities,
        }
    }

    pub fn in_memory_for_tests(
        session_id: impl Into<String>,
        extension_id: impl Into<String>,
    ) -> Self {
        Self::new(
            Arc::new(
                SqliteExtensionStateStore::try_open_in_memory().expect("extension state store"),
            ),
            session_id,
            extension_id,
            Vec::new(),
        )
    }

    pub fn for_extension(
        &self,
        extension_id: impl Into<String>,
        capabilities: Vec<ExtensionCapability>,
    ) -> Self {
        Self {
            store: Arc::clone(&self.store),
            session_id: self.session_id.clone(),
            extension_id: extension_id.into(),
            capabilities,
        }
    }

    pub fn store(&self) -> Arc<dyn ExtensionStateStore> {
        Arc::clone(&self.store)
    }

    pub fn append_entry(
        &self,
        key: &str,
        value: Value,
        branch_policy: BranchPolicy,
    ) -> std::result::Result<(), ExtensionStateError> {
        self.store
            .append_entry(
                &self.session_id,
                &self.extension_id,
                key,
                value,
                branch_policy,
            )
            .map_err(ExtensionStateError::Store)
    }

    pub fn get_entries(
        &self,
        prefix: &str,
    ) -> std::result::Result<Vec<ExtensionStateEntry>, ExtensionStateError> {
        self.store
            .get_entries(&self.session_id, &self.extension_id, prefix)
            .map_err(ExtensionStateError::Store)
    }

    pub fn write_to_session(
        &self,
        session_id: &str,
        key: &str,
        value: Value,
    ) -> std::result::Result<(), ExtensionStateError> {
        if !self
            .capabilities
            .contains(&ExtensionCapability::CrossSessionState)
        {
            return Err(ExtensionStateError::MissingCrossSessionCapability {
                extension_id: self.extension_id.clone(),
            });
        }
        self.store
            .append_entry(
                session_id,
                &self.extension_id,
                key,
                value,
                BranchPolicy::Fork,
            )
            .map_err(ExtensionStateError::Store)
    }
}

pub struct SqliteExtensionStateStore {
    conn: Mutex<Connection>,
}

pub struct ExtensionStateReaperHook {
    store: Arc<dyn ExtensionStateStore>,
}

impl ExtensionStateReaperHook {
    pub fn new(store: Arc<dyn ExtensionStateStore>) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl HookHandler for ExtensionStateReaperHook {
    fn name(&self) -> &str {
        "extension-state-reaper"
    }

    async fn session_end(&self, session_id: &str, _total_turns: u32) {
        if let Err(e) = self.store.reap_session(session_id) {
            tracing::warn!(
                session_id,
                error = %e,
                "failed to reap extension state on session_end"
            );
        }
    }
}

impl SqliteExtensionStateStore {
    pub fn try_new() -> Result<Self> {
        let dir = crate::config::vulcan_home();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create vulcan_home at {}", dir.display()))?;
        Self::try_open_at(&dir.join("extension_state.db"))
    }

    pub fn try_open_at(path: &std::path::Path) -> Result<Self> {
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
            CREATE TABLE IF NOT EXISTS extension_state (
                session_id      TEXT NOT NULL,
                extension_id    TEXT NOT NULL,
                key             TEXT NOT NULL,
                value_json      TEXT,
                branch_policy   TEXT NOT NULL,
                ref_session_id  TEXT,
                ref_extension_id TEXT,
                ref_key         TEXT,
                PRIMARY KEY (session_id, extension_id, key)
            );

            CREATE INDEX IF NOT EXISTS idx_extension_state_lookup
            ON extension_state(session_id, extension_id, key);
            "#,
        )?;
        Ok(())
    }

    fn resolve_target(
        conn: &Connection,
        session_id: &str,
        extension_id: &str,
        key: &str,
    ) -> Result<(String, String, String)> {
        let row = conn
            .query_row(
                "SELECT ref_session_id, ref_extension_id, ref_key
                 FROM extension_state
                 WHERE session_id = ?1 AND extension_id = ?2 AND key = ?3",
                params![session_id, extension_id, key],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        match row {
            Some((Some(s), Some(e), Some(k))) => Ok((s, e, k)),
            _ => Ok((
                session_id.to_string(),
                extension_id.to_string(),
                key.to_string(),
            )),
        }
    }

    fn row_to_entry(
        session_id: String,
        extension_id: String,
        key: String,
        value_json: String,
        branch_policy: String,
    ) -> Result<ExtensionStateEntry> {
        Ok(ExtensionStateEntry {
            session_id,
            extension_id,
            key,
            value: serde_json::from_str(&value_json)?,
            branch_policy: branch_policy.parse().unwrap_or_default(),
        })
    }
}

impl ExtensionStateStore for SqliteExtensionStateStore {
    fn append_entry(
        &self,
        session_id: &str,
        extension_id: &str,
        key: &str,
        value: Value,
        branch_policy: BranchPolicy,
    ) -> Result<()> {
        let value_json = serde_json::to_string(&value)?;
        let conn = self.conn.lock();
        let (target_session, target_extension, target_key) =
            Self::resolve_target(&conn, session_id, extension_id, key)?;
        conn.execute(
            "INSERT INTO extension_state
                (session_id, extension_id, key, value_json, branch_policy,
                 ref_session_id, ref_extension_id, ref_key)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, NULL)
             ON CONFLICT(session_id, extension_id, key) DO UPDATE SET
                value_json = excluded.value_json,
                branch_policy = excluded.branch_policy,
                ref_session_id = NULL,
                ref_extension_id = NULL,
                ref_key = NULL",
            params![
                target_session,
                target_extension,
                target_key,
                value_json,
                branch_policy.as_str()
            ],
        )?;
        Ok(())
    }

    fn get_entries(
        &self,
        session_id: &str,
        extension_id: &str,
        prefix: &str,
    ) -> Result<Vec<ExtensionStateEntry>> {
        let conn = self.conn.lock();
        let like = format!("{prefix}%");
        let mut stmt = conn.prepare(
            "SELECT child.session_id, child.extension_id, child.key,
                    COALESCE(target.value_json, child.value_json) AS value_json,
                    COALESCE(target.branch_policy, child.branch_policy) AS branch_policy
             FROM extension_state child
             LEFT JOIN extension_state target
                ON child.ref_session_id = target.session_id
               AND child.ref_extension_id = target.extension_id
               AND child.ref_key = target.key
             WHERE child.session_id = ?1
               AND child.extension_id = ?2
               AND child.key LIKE ?3
               AND COALESCE(target.value_json, child.value_json) IS NOT NULL
             ORDER BY child.key ASC",
        )?;
        let rows: Vec<_> = stmt
            .query_map(params![session_id, extension_id, like], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;
        rows.into_iter()
            .map(|row| Self::row_to_entry(row.0, row.1, row.2, row.3, row.4))
            .collect()
    }

    fn branch_session(
        &self,
        parent_session_id: &str,
        child_session_id: &str,
        active_extension_ids: &[String],
    ) -> Result<usize> {
        if active_extension_ids.is_empty() {
            return Ok(0);
        }
        let conn = self.conn.lock();
        let mut copied = 0usize;
        for extension_id in active_extension_ids {
            let mut stmt = conn.prepare(
                "SELECT key, value_json, branch_policy
                 FROM extension_state
                 WHERE session_id = ?1 AND extension_id = ?2",
            )?;
            let rows: Vec<_> = stmt
                .query_map(params![parent_session_id, extension_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<std::result::Result<_, _>>()?;
            drop(stmt);
            for (key, value_json, policy) in rows {
                match policy.parse::<BranchPolicy>().unwrap_or_default() {
                    BranchPolicy::Fork => {
                        conn.execute(
                            "INSERT OR REPLACE INTO extension_state
                                (session_id, extension_id, key, value_json, branch_policy,
                                 ref_session_id, ref_extension_id, ref_key)
                             VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, NULL)",
                            params![
                                child_session_id,
                                extension_id,
                                key,
                                value_json,
                                BranchPolicy::Fork.as_str()
                            ],
                        )?;
                        copied += 1;
                    }
                    BranchPolicy::InheritRef => {
                        conn.execute(
                            "INSERT OR REPLACE INTO extension_state
                                (session_id, extension_id, key, value_json, branch_policy,
                                 ref_session_id, ref_extension_id, ref_key)
                             VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7)",
                            params![
                                child_session_id,
                                extension_id,
                                key,
                                BranchPolicy::InheritRef.as_str(),
                                parent_session_id,
                                extension_id,
                                key,
                            ],
                        )?;
                        copied += 1;
                    }
                    BranchPolicy::Isolate => {}
                }
            }
        }
        Ok(copied)
    }

    fn reap_session(&self, session_id: &str) -> Result<usize> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "DELETE FROM extension_state WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active_exts() -> Vec<String> {
        vec!["todo".to_string()]
    }

    #[test]
    fn fork_policy_deep_copies_parent_rows() {
        let store = SqliteExtensionStateStore::try_open_in_memory().unwrap();
        store
            .append_entry(
                "parent",
                "todo",
                "items/a",
                Value::from("a"),
                BranchPolicy::Fork,
            )
            .unwrap();
        assert_eq!(
            store
                .branch_session("parent", "child", &active_exts())
                .unwrap(),
            1
        );
        store
            .append_entry(
                "child",
                "todo",
                "items/a",
                Value::from("child"),
                BranchPolicy::Fork,
            )
            .unwrap();

        let parent = store.get_entries("parent", "todo", "items/").unwrap();
        let child = store.get_entries("child", "todo", "items/").unwrap();
        assert_eq!(parent[0].value, Value::from("a"));
        assert_eq!(child[0].value, Value::from("child"));
    }

    #[test]
    fn inherit_ref_policy_shares_parent_row() {
        let store = SqliteExtensionStateStore::try_open_in_memory().unwrap();
        store
            .append_entry(
                "parent",
                "todo",
                "items/a",
                Value::from("a"),
                BranchPolicy::InheritRef,
            )
            .unwrap();
        store
            .branch_session("parent", "child", &active_exts())
            .unwrap();
        store
            .append_entry(
                "child",
                "todo",
                "items/a",
                Value::from("shared"),
                BranchPolicy::InheritRef,
            )
            .unwrap();

        let parent = store.get_entries("parent", "todo", "items/").unwrap();
        let child = store.get_entries("child", "todo", "items/").unwrap();
        assert_eq!(parent[0].value, Value::from("shared"));
        assert_eq!(child[0].value, Value::from("shared"));
    }

    #[test]
    fn isolate_policy_drops_parent_rows_for_child() {
        let store = SqliteExtensionStateStore::try_open_in_memory().unwrap();
        store
            .append_entry(
                "parent",
                "todo",
                "items/a",
                Value::from("a"),
                BranchPolicy::Isolate,
            )
            .unwrap();
        assert_eq!(
            store
                .branch_session("parent", "child", &active_exts())
                .unwrap(),
            0
        );
        assert!(
            store
                .get_entries("child", "todo", "items/")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn cross_session_write_requires_capability() {
        let store: Arc<dyn ExtensionStateStore> =
            Arc::new(SqliteExtensionStateStore::try_open_in_memory().unwrap());
        let denied = ExtensionStateContext::new(Arc::clone(&store), "child", "todo", Vec::new());
        let err = denied
            .write_to_session("parent", "handoff", Value::from("x"))
            .unwrap_err();
        assert!(matches!(
            err,
            ExtensionStateError::MissingCrossSessionCapability { .. }
        ));

        let allowed = ExtensionStateContext::new(
            Arc::clone(&store),
            "child",
            "todo",
            vec![ExtensionCapability::CrossSessionState],
        );
        allowed
            .write_to_session("parent", "handoff", Value::from("x"))
            .unwrap();
        let rows = store.get_entries("parent", "todo", "handoff").unwrap();
        assert_eq!(rows[0].value, Value::from("x"));
    }

    #[test]
    fn reap_session_deletes_child_rows_only() {
        let store = SqliteExtensionStateStore::try_open_in_memory().unwrap();
        store
            .append_entry("parent", "todo", "k", Value::from("p"), BranchPolicy::Fork)
            .unwrap();
        store
            .append_entry("child", "todo", "k", Value::from("c"), BranchPolicy::Fork)
            .unwrap();
        assert_eq!(store.reap_session("child").unwrap(), 1);
        assert!(store.get_entries("child", "todo", "").unwrap().is_empty());
        assert_eq!(
            store.get_entries("parent", "todo", "").unwrap()[0].value,
            Value::from("p")
        );
    }
}
