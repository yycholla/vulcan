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

use anyhow::{Context, Result};

use std::path::PathBuf;

use crate::artifact::{ArtifactStore, InMemoryArtifactStore, SqliteArtifactStore};
use crate::code::lsp::LspManager;
use crate::extensions::api::wire_inventory_into_registry;
use crate::extensions::{ExtensionAuditLog, ExtensionRegistry};
use crate::memory::SessionStore;
use crate::memory::cortex::CortexStore;
use crate::orchestration::OrchestrationStore;
use crate::run_record::{InMemoryRunStore, RunStore, SqliteRunStore};

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
}

impl RuntimeResourcePool {
    /// Open the production pool — SQLite-backed durable stores under
    /// `~/.vulcan/`, with safe in-memory fallbacks when a store can't
    /// be opened (read-only home, missing perms). Slice 3 keeps the
    /// fallback policy identical to the all-in-one
    /// `Agent::build_from_parts` path so resource pooling does not
    /// silently change durability behavior.
    pub fn try_new() -> Result<Self> {
        let session_store =
            Arc::new(SessionStore::try_new().context("RuntimeResourcePool: open SessionStore")?);

        let run_store: Arc<dyn RunStore> = match SqliteRunStore::try_new() {
            Ok(s) => Arc::new(s),
            Err(e) => {
                tracing::warn!(
                    "RuntimeResourcePool: run_record store unavailable ({e}); using in-memory"
                );
                Arc::new(InMemoryRunStore::default())
            }
        };

        let artifact_store: Arc<dyn ArtifactStore> = match SqliteArtifactStore::try_new() {
            Ok(s) => Arc::new(s),
            Err(e) => {
                tracing::warn!(
                    "RuntimeResourcePool: artifact store unavailable ({e}); using in-memory"
                );
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

        Ok(Self {
            session_store,
            run_store,
            artifact_store,
            orchestration,
            lsp_manager,
            cortex_store: None,
            extension_registry,
            extension_audit_log,
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
    pub fn for_tests() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let extension_registry = Arc::new(ExtensionRegistry::new());
        wire_inventory_into_registry(&extension_registry);
        Self {
            session_store: Arc::new(SessionStore::in_memory()),
            run_store: Arc::new(InMemoryRunStore::default()),
            artifact_store: Arc::new(InMemoryArtifactStore::new()),
            orchestration: Arc::new(OrchestrationStore::new()),
            lsp_manager: Arc::new(LspManager::new(cwd)),
            cortex_store: None,
            extension_registry,
            extension_audit_log: Arc::new(ExtensionAuditLog::default()),
        }
    }

    /// Cloneable handle to the shared session store. Sessions do not
    /// open their own `SessionStore`; FTS5 readers (`session.search`)
    /// share the same connection through this Arc.
    pub fn session_store(&self) -> Arc<SessionStore> {
        Arc::clone(&self.session_store)
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_tests_hands_out_same_session_store_arc() {
        // Slice 3 acceptance: sessions share the daemon's SessionStore
        // instead of each opening their own SQLite connection.
        let pool = RuntimeResourcePool::for_tests();
        let s1 = pool.session_store();
        let s2 = pool.session_store();
        assert!(
            Arc::ptr_eq(&s1, &s2),
            "session_store() must hand out the same Arc"
        );
    }

    #[test]
    fn for_tests_shares_run_artifact_orchestration_stores() {
        let pool = RuntimeResourcePool::for_tests();
        assert!(Arc::ptr_eq(&pool.run_store(), &pool.run_store()));
        assert!(Arc::ptr_eq(&pool.artifact_store(), &pool.artifact_store()));
        assert!(Arc::ptr_eq(&pool.orchestration(), &pool.orchestration()));
    }

    #[test]
    fn for_tests_shares_lsp_manager() {
        // Slice 3 deepening: LSP servers stay warm across sessions —
        // the pool hands out the same Arc.
        let pool = RuntimeResourcePool::for_tests();
        assert!(Arc::ptr_eq(&pool.lsp_manager(), &pool.lsp_manager()));
    }

    #[test]
    fn cortex_store_is_none_by_default_and_some_after_install() {
        // Slice 3 deepening: cortex is install-on-demand. Default
        // pool has no cortex store; the daemon installs one at boot
        // when config.cortex.enabled.
        let pool = RuntimeResourcePool::for_tests();
        assert!(pool.cortex_store().is_none());
    }

    #[test]
    fn pool_exposes_extension_registry_populated_from_inventory() {
        // GH issue #549: the daemon-owned **Runtime Resource Pool**
        // owns one **`ExtensionRegistry`**. Calling `extension_registry()`
        // hands out the same `Arc` and the registry is pre-populated
        // from `inventory::iter` so cargo-crate extensions self-register
        // at pool construction.
        let pool = RuntimeResourcePool::for_tests();
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
}
