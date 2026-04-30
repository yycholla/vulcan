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

use crate::artifact::{ArtifactStore, InMemoryArtifactStore, SqliteArtifactStore};
use crate::memory::SessionStore;
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

        Ok(Self {
            session_store,
            run_store,
            artifact_store,
            orchestration,
        })
    }

    /// Test-only constructor with in-memory backends. Production
    /// callers always go through [`Self::try_new`].
    #[doc(hidden)]
    pub fn for_tests() -> Self {
        Self {
            session_store: Arc::new(SessionStore::in_memory()),
            run_store: Arc::new(InMemoryRunStore::default()),
            artifact_store: Arc::new(InMemoryArtifactStore::new()),
            orchestration: Arc::new(OrchestrationStore::new()),
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
}
