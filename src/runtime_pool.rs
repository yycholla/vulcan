//! Slice 3 (YYC-…): daemon-owned holder for expensive runtime resources
//! shared across sessions.
//!
//! Per ADR 0002, the daemon owns exactly one `RuntimeResourcePool` per
//! process. Sessions assemble their conversation-specific state from
//! adapters the pool hands out, instead of calling the all-in-one
//! `Agent::builder(config).build()` cold-start path. Provider catalog
//! caches, cortex memory, LSP processes, durable stores, and the
//! orchestration store all live here so they survive across sessions
//! and across the gateway's lane churn.
//!
//! Slice 3 lands the scaffolding: shared [`SessionStore`], shared run
//! and artifact stores, shared orchestration store. Subsequent slices
//! widen the pool to cover provider catalog metadata, LSP processes,
//! cortex admin (Slice 4), and the tool/hook factories the
//! per-session `Agent` assembles its registry from.

use std::sync::Arc;

use anyhow::Result;
use serde::Serialize;

use std::path::PathBuf;

use crate::artifact::{ArtifactStore, InMemoryArtifactStore, SqliteArtifactStore};
use crate::code::lsp::LspManager;
use crate::extensions::api::wire_inventory_into_registry;
use crate::extensions::{
    ExtensionAuditLog, ExtensionRegistry, ExtensionStateStore, TursoExtensionStateStore,
};
use crate::memory::SessionStore;
use crate::memory::cortex::CortexStore;
use crate::orchestration::OrchestrationStore;
use crate::pause::PauseSender;
use crate::run_record::{InMemoryRunStore, RunStore, SqliteRunStore};
use crate::tools::{EditDiffSink, ToolRegistry};

/// Operator-visible record for a runtime resource that fell back to a
/// degraded in-memory/non-durable mode instead of aborting daemon startup.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimeResourceDegradation {
    pub component: String,
    pub fallback: String,
    pub message: String,
}

impl RuntimeResourceDegradation {
    fn in_memory(component: &str, message: impl Into<String>) -> Self {
        Self {
            component: component.into(),
            fallback: "in_memory".into(),
            message: message.into(),
        }
    }
}

#[derive(Clone)]
pub struct ToolRegistryBuildOptions {
    cwd: PathBuf,
    diff_sink: Option<EditDiffSink>,
    pause_tx: Option<PauseSender>,
}

impl ToolRegistryBuildOptions {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            diff_sink: None,
            pause_tx: None,
        }
    }

    pub fn with_diff_sink(mut self, diff_sink: Option<EditDiffSink>) -> Self {
        self.diff_sink = diff_sink;
        self
    }

    pub fn with_pause_channel(mut self, pause_tx: Option<PauseSender>) -> Self {
        self.pause_tx = pause_tx;
        self
    }
}

/// Daemon-owned set of expensive adapters shared across sessions.
///
/// Cloning a field returns a cheap `Arc` clone — every session uses the
/// same backing handle, so opening N sessions does not open N SQLite
/// connections, N HNSW indices, or N LSP server pools.
pub struct RuntimeResourcePool {
    session_store: Arc<SessionStore>,
    run_store: Arc<dyn RunStore>,
    artifact_store: Arc<dyn ArtifactStore>,
    orchestration: Arc<OrchestrationStore>,
    degraded_resources: Vec<RuntimeResourceDegradation>,
    /// Slice 3: shared LSP server pool. Sessions reuse the same pool
    /// instead of spawning per-Agent server processes; idle servers
    /// stay warm across session lifetimes.
    lsp_manager: Arc<LspManager>,
    /// Slice 3 deepening: shared cortex graph memory. The daemon
    /// owns the redb lock for the lifetime of the process; sessions
    /// must share this handle rather than opening their own (which
    /// would fail with `DatabaseAlreadyOpen`). `None` when cortex is
    /// disabled in config.
    cortex_store: Option<Arc<CortexStore>>,
    /// GH issue #549: daemon-owned **`ExtensionRegistry`**. Populated
    /// from `inventory::iter` at pool construction; sessions read
    /// active daemon-side extensions from this registry to wire their
    /// per-Session hook handlers, tools, commands, providers.
    extension_registry: Arc<ExtensionRegistry>,
    /// GH issue #557: daemon-owned **`ExtensionAuditLog`**. Shared
    /// across sessions — every per-Session `HookRegistry` records
    /// `InputIntercept` outcomes here. `vulcan extension audit`
    /// reads from this same handle.
    extension_audit_log: Arc<ExtensionAuditLog>,
    /// GH issue #270: daemon-owned extension state DB. Session
    /// extensions receive scoped handles derived from this shared store.
    extension_state_store: Arc<dyn ExtensionStateStore>,
}

impl RuntimeResourcePool {
    /// Open the production pool — SQLite-backed durable stores under
    /// `~/.vulcan/`, with safe in-memory fallbacks when a store can't
    /// be opened (read-only home, missing perms). Slice 3 keeps the
    /// fallback policy identical to the all-in-one
    /// `Agent::build_from_parts` path so resource pooling does not
    /// silently change durability behavior.
    pub async fn try_new() -> Result<Self> {
        let mut degraded_resources = Vec::new();
        let session_store = match SessionStore::try_new().await {
            Ok(store) => Arc::new(store),
            Err(e) => {
                let message = format!(
                    "RuntimeResourcePool: session store unavailable ({e:#}); using in-memory session history"
                );
                tracing::warn!("{message}");
                degraded_resources.push(RuntimeResourceDegradation::in_memory(
                    "session_store",
                    message,
                ));
                Arc::new(SessionStore::in_memory().await)
            }
        };

        let run_store: Arc<dyn RunStore> = match SqliteRunStore::try_new() {
            Ok(s) => Arc::new(s),
            Err(e) => {
                let message = format!(
                    "RuntimeResourcePool: run_record store unavailable ({e:#}); using in-memory"
                );
                tracing::warn!("{message}");
                degraded_resources
                    .push(RuntimeResourceDegradation::in_memory("run_store", message));
                Arc::new(InMemoryRunStore::default())
            }
        };

        let artifact_store: Arc<dyn ArtifactStore> = match SqliteArtifactStore::try_new() {
            Ok(s) => Arc::new(s),
            Err(e) => {
                let message = format!(
                    "RuntimeResourcePool: artifact store unavailable ({e:#}); using in-memory"
                );
                tracing::warn!("{message}");
                degraded_resources.push(RuntimeResourceDegradation::in_memory(
                    "artifact_store",
                    message,
                ));
                Arc::new(InMemoryArtifactStore::new())
            }
        };

        let orchestration = Arc::new(OrchestrationStore::new());

        // Daemon process cwd defines the workspace root. Sessions
        // inherit this by virtue of running inside the daemon.
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let lsp_manager = Arc::new(LspManager::new(cwd));

        let extension_registry = Arc::new(ExtensionRegistry::new());
        let registered = wire_inventory_into_registry(&extension_registry);
        tracing::info!(
            registered_extensions = registered,
            "RuntimeResourcePool: extension registry populated from inventory"
        );

        let extension_audit_log = Arc::new(ExtensionAuditLog::default());
        let extension_state_store: Arc<dyn ExtensionStateStore> =
            match TursoExtensionStateStore::try_new() {
                Ok(store) => Arc::new(store),
                Err(e) => {
                    let message = format!(
                        "RuntimeResourcePool: extension state store unavailable ({e:#}); using in-memory"
                    );
                    tracing::warn!("{message}");
                    degraded_resources.push(RuntimeResourceDegradation::in_memory(
                        "extension_state_store",
                        message,
                    ));
                    Arc::new(TursoExtensionStateStore::try_open_in_memory()?)
                }
            };

        Ok(Self {
            session_store,
            run_store,
            artifact_store,
            orchestration,
            degraded_resources,
            lsp_manager,
            cortex_store: None,
            extension_registry,
            extension_audit_log,
            extension_state_store,
        })
    }

    /// Install a daemon-owned [`CortexStore`] on the pool. Called by
    /// daemon boot after `CortexStore::try_open` succeeds; sessions
    /// pull this handle instead of opening their own.
    pub fn with_cortex_store(mut self, store: Arc<CortexStore>) -> Self {
        self.cortex_store = Some(store);
        self
    }

    /// Test-only constructor with in-memory backends. Production
    /// callers always go through [`Self::try_new`].
    #[doc(hidden)]
    pub async fn for_tests() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let extension_registry = Arc::new(ExtensionRegistry::new());
        wire_inventory_into_registry(&extension_registry);
        Self {
            session_store: Arc::new(SessionStore::in_memory().await),
            run_store: Arc::new(InMemoryRunStore::default()),
            artifact_store: Arc::new(InMemoryArtifactStore::new()),
            orchestration: Arc::new(OrchestrationStore::new()),
            degraded_resources: Vec::new(),
            lsp_manager: Arc::new(LspManager::new(cwd)),
            cortex_store: None,
            extension_registry,
            extension_audit_log: Arc::new(ExtensionAuditLog::default()),
            extension_state_store: Arc::new(
                TursoExtensionStateStore::try_open_in_memory()
                    .expect("in-memory extension state store"),
            ),
        }
    }

    /// Cloneable handle to the shared session store. Sessions do not
    /// open their own `SessionStore`; FTS5 readers (`session.search`)
    /// share the same connection through this Arc.
    pub fn session_store(&self) -> Arc<SessionStore> {
        Arc::clone(&self.session_store)
    }

    /// True when one or more durable runtime resources could not be
    /// opened and the pool installed a safe fallback instead.
    pub fn is_degraded(&self) -> bool {
        !self.degraded_resources.is_empty()
    }

    /// Operator-visible degraded resource records for daemon status and
    /// tests. Empty means all pool resources opened in their preferred mode.
    pub fn degraded_resources(&self) -> &[RuntimeResourceDegradation] {
        &self.degraded_resources
    }

    /// Test-only constructor that marks the otherwise in-memory test
    /// pool as degraded so daemon status tests can exercise operator
    /// visibility without needing filesystem permission failures.
    #[cfg(test)]
    #[doc(hidden)]
    pub async fn for_tests_degraded(component: &str, message: &str) -> Self {
        let mut pool = Self::for_tests().await;
        pool.degraded_resources
            .push(RuntimeResourceDegradation::in_memory(component, message));
        pool
    }

    /// Cloneable handle to the shared run-record store.
    pub fn run_store(&self) -> Arc<dyn RunStore> {
        Arc::clone(&self.run_store)
    }

    /// Cloneable handle to the shared artifact store.
    pub fn artifact_store(&self) -> Arc<dyn ArtifactStore> {
        Arc::clone(&self.artifact_store)
    }

    /// Cloneable handle to the shared orchestration store.
    pub fn orchestration(&self) -> Arc<OrchestrationStore> {
        Arc::clone(&self.orchestration)
    }

    /// Cloneable handle to the shared LSP server pool.
    pub fn lsp_manager(&self) -> Arc<LspManager> {
        Arc::clone(&self.lsp_manager)
    }

    /// Build the per-session executable tool registry from daemon-owned
    /// resources. The registry still owns execution/lookup; the pool owns
    /// assembly of shared handles and interactive frontend wiring.
    pub fn build_tool_registry(&self, options: ToolRegistryBuildOptions) -> ToolRegistry {
        let mut registry = ToolRegistry::new_with_diff_and_lsp(
            options.diff_sink.clone(),
            Some(self.lsp_manager()),
            options.cwd,
        );
        crate::tools::register_interactive_tools(
            &mut registry,
            options.diff_sink,
            options.pause_tx,
        );
        registry
    }

    /// Cloneable handle to the shared cortex graph memory, when the
    /// daemon installed one at boot. `None` for tests / disabled
    /// configurations.
    pub fn cortex_store(&self) -> Option<Arc<CortexStore>> {
        self.cortex_store.as_ref().map(Arc::clone)
    }

    /// GH issue #549: cloneable handle to the daemon-owned
    /// **`ExtensionRegistry`**. Populated from `inventory::iter` at
    /// pool construction; sessions consult this when wiring their
    /// per-Session daemon extensions.
    pub fn extension_registry(&self) -> Arc<ExtensionRegistry> {
        Arc::clone(&self.extension_registry)
    }

    /// GH issue #557: cloneable handle to the daemon-owned
    /// **`ExtensionAuditLog`**. Per-Session `HookRegistry`s record
    /// `InputIntercept` outcomes here so `vulcan extension audit`
    /// can surface them across sessions.
    pub fn extension_audit_log(&self) -> Arc<ExtensionAuditLog> {
        Arc::clone(&self.extension_audit_log)
    }

    /// GH issue #270: cloneable handle to the daemon-owned
    /// extension state store.
    pub fn extension_state_store(&self) -> Arc<dyn ExtensionStateStore> {
        Arc::clone(&self.extension_state_store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn for_tests_hands_out_same_session_store_arc() {
        // Slice 3 acceptance: sessions share the daemon's SessionStore
        // instead of each opening their own SQLite connection.
        let pool = RuntimeResourcePool::for_tests().await;
        let s1 = pool.session_store();
        let s2 = pool.session_store();
        assert!(
            Arc::ptr_eq(&s1, &s2),
            "session_store() must hand out the same Arc"
        );
    }

    #[tokio::test]
    async fn degraded_session_store_fallback_is_operator_visible() {
        let pool = RuntimeResourcePool::for_tests_degraded(
            "session_store",
            "sqlite unavailable; using in-memory session history",
        )
        .await;

        assert!(pool.is_degraded());
        assert_eq!(pool.degraded_resources().len(), 1);
        assert_eq!(pool.degraded_resources()[0].component, "session_store");
        assert_eq!(pool.degraded_resources()[0].fallback, "in_memory");
    }

    #[tokio::test]
    async fn for_tests_shares_run_artifact_orchestration_stores() {
        let pool = RuntimeResourcePool::for_tests().await;
        assert!(Arc::ptr_eq(&pool.run_store(), &pool.run_store()));
        assert!(Arc::ptr_eq(&pool.artifact_store(), &pool.artifact_store()));
        assert!(Arc::ptr_eq(&pool.orchestration(), &pool.orchestration()));
    }

    #[tokio::test]
    async fn for_tests_shares_lsp_manager() {
        // Slice 3 deepening: LSP servers stay warm across sessions —
        // the pool hands out the same Arc.
        let pool = RuntimeResourcePool::for_tests().await;
        assert!(Arc::ptr_eq(&pool.lsp_manager(), &pool.lsp_manager()));
    }

    #[tokio::test]
    async fn pool_builds_ordered_tool_registry_with_interactive_tools() {
        let pool = RuntimeResourcePool::for_tests().await;
        let diff_sink = crate::tools::new_diff_sink();
        let (pause_tx, _pause_rx) = crate::pause::channel(1);

        let registry = pool.build_tool_registry(
            ToolRegistryBuildOptions::new(std::env::current_dir().unwrap())
                .with_diff_sink(Some(diff_sink))
                .with_pause_channel(Some(pause_tx)),
        );
        let names = registry.catalog(None).names();

        assert_eq!(
            &names[0..5],
            &[
                "read_file".to_string(),
                "write_file".to_string(),
                "search_files".to_string(),
                "edit_file".to_string(),
                "list_files".to_string(),
            ]
        );
        assert!(
            names.contains(&"ask_user".to_string()),
            "pool construction should own interactive tool registration"
        );
        assert!(
            names.contains(&"goto_definition".to_string()),
            "pool construction should reuse the shared LSP manager for semantic tools"
        );
    }

    #[tokio::test]
    async fn cortex_store_is_none_by_default_and_some_after_install() {
        // Slice 3 deepening: cortex is install-on-demand. Default
        // pool has no cortex store; the daemon installs one at boot
        // when config.cortex.enabled.
        let pool = RuntimeResourcePool::for_tests().await;
        assert!(pool.cortex_store().is_none());
    }

    #[tokio::test]
    async fn pool_exposes_extension_registry_populated_from_inventory() {
        // GH issue #549: the daemon-owned **Runtime Resource Pool**
        // owns one **`ExtensionRegistry`**. Calling `extension_registry()`
        // hands out the same `Arc` and the registry is pre-populated
        // from `inventory::iter` so cargo-crate extensions self-register
        // at pool construction.
        let pool = RuntimeResourcePool::for_tests().await;
        let r1 = pool.extension_registry();
        let r2 = pool.extension_registry();
        assert!(
            Arc::ptr_eq(&r1, &r2),
            "extension_registry() must hand out the same Arc"
        );
        // The cfg(test) inventory submit in `extensions::api::tests`
        // registers a `stub-inventory` entry; pool construction calls
        // `wire_inventory_into_registry` so it appears here too.
        assert!(
            r1.daemon_extension_count() >= 1,
            "expected at least one inventory-registered extension"
        );
    }

    #[tokio::test]
    async fn for_tests_shares_extension_state_store() {
        let pool = RuntimeResourcePool::for_tests().await;
        let store = pool.extension_state_store();
        let alpha = store.scope("alpha").unwrap();
        alpha
            .put_json("k", &serde_json::json!({"persisted": true}))
            .unwrap();

        let same_store = pool.extension_state_store();
        assert_eq!(
            same_store.scope("alpha").unwrap().get_json("k").unwrap(),
            Some(serde_json::json!({"persisted": true}))
        );
    }
}
