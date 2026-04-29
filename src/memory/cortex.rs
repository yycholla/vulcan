//! Embedded graph memory for Vulcan, wrapping `cortex-memory-core`.
//!
//! Provides a `CortexStore` that owns the Cortex graph database (redb-backed)
//! and exposes core operations: store facts, semantic search, graph traversal,
//! and edge management.
//!
//! All public methods are `&self` — `Cortex` and its storage use interior
//! mutability internally, so callers can pass `Arc<CortexStore>` through
//! hooks without locking.
//!
//! ## Why no persistent second `RedbStorage` handle?
//!
//! `Cortex::open()` opens `cortex.redb` once and holds an exclusive file lock.
//! Opening `RedbStorage::open()` a second time on the same file fails with
//! "Database already open. Cannot acquire lock." because redb uses a
//! single-writer lock model.
//!
//! Instead, edge-management and decay operations that need the raw storage
//! API open a **transient** `RedbStorage` handle on demand (the CLI commands
//! that call these methods are short-lived — they open, do work, and exit,
//! so the lock is held only briefly). The long-lived `CortexStore` held by
//! the agent and hooks never holds a second handle, so the lock is free
//! when the TUI exits.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use cortex_core::{
    Cortex, Edge, EdgeProvenance, LibraryConfig, Node, NodeFilter, NodeId, Relation,
    linker::{DecayConfig, DecayEngine},
    storage::{RedbStorage, Storage},
};

use crate::config::{CortexConfig, vulcan_home};

/// Wrapper around the embedded Cortex graph memory engine.
pub struct CortexStore {
    pub inner: Cortex,
    config: CortexConfig,
}

impl CortexStore {
    /// Open (or create) the cortex.redb database.
    ///
    /// Failures are non-fatal — the caller logs and continues without cortex.
    pub fn try_open(config: &CortexConfig) -> Result<Arc<Self>> {
        let db_path = resolve_db_path(config)?;
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create cortex db dir {}", parent.display()))?;
        }

        let lib_config = LibraryConfig {
            embedding_model: config.embedding_model.clone(),
            ..LibraryConfig::default()
        };

        let inner = Cortex::open(&db_path, lib_config)
            .with_context(|| format!("open cortex db at {}", db_path.display()))?;

        tracing::info!(
            "cortex memory ready at {} (model: {})",
            db_path.display(),
            config.embedding_model,
        );

        Ok(Arc::new(Self {
            inner,
            config: config.clone(),
        }))
    }

    // ── Convenience node constructors ──

    /// Create a `fact` node tagged with Vulcan as the source agent.
    pub fn fact(title: &str, importance: f32) -> Node {
        Cortex::fact(title, importance)
    }

    /// Create a `fact` node with explicit body text.
    pub fn fact_with_body(title: &str, body: &str, importance: f32) -> Node {
        let mut node = Cortex::fact(title, importance);
        node.data.body = body.to_string();
        node
    }

    /// Create a `decision` node.
    pub fn decision(title: &str, body: &str, importance: f32) -> Node {
        Cortex::decision(title, body, importance)
    }

    /// Create a `pattern` node.
    pub fn pattern(title: &str, body: &str, importance: f32) -> Node {
        Cortex::pattern(title, body, importance)
    }

    // ── CRUD ──

    /// Store a node with auto-generated embedding.
    pub fn store(&self, node: Node) -> Result<NodeId> {
        self.inner.store(node).context("cortex store node")
    }

    /// Retrieve a node by ID.
    pub fn get_node(&self, id: NodeId) -> Result<Option<Node>> {
        self.inner.get_node(id).context("cortex get node")
    }

    /// List nodes matching a filter.
    pub fn list_nodes(&self, filter: NodeFilter) -> Result<Vec<Node>> {
        self.inner.list_nodes(filter).context("cortex list nodes")
    }

    /// Create a directed edge between two nodes.
    pub fn create_edge(&self, from: NodeId, to: NodeId, relation: &str, weight: f32) -> Result<()> {
        let relation = Relation::new(relation)
            .map_err(|e| anyhow::anyhow!("invalid relation {relation}: {e}"))?;
        let edge = Edge::new(
            from,
            to,
            relation,
            weight,
            EdgeProvenance::Manual {
                created_by: "vulcan".into(),
            },
        );
        self.inner.create_edge(edge).context("cortex create edge")
    }

    // ── Edge management (requires transient storage handle) ──
    //
    // These methods open a short-lived `RedbStorage` handle because
    // `Cortex` does not expose the raw storage API. They are intended
    // for CLI subcommands that run and exit — NOT for the agent loop.
    // The transient handle is opened, the operation runs, and the
    // handle is dropped immediately, releasing the file lock.

    /// List all edges originating from `node_id`.
    ///
    /// Opens a transient storage handle. Only call from short-lived
    /// CLI commands, not from agent hooks.
    pub fn edges_from(&self, node_id: NodeId) -> Result<Vec<cortex_core::Edge>> {
        let storage = open_transient_storage(&self.config)?;
        storage.edges_from(node_id).context("cortex edges_from")
    }

    /// List all edges terminating at `node_id`.
    ///
    /// Opens a transient storage handle. Only call from short-lived
    /// CLI commands, not from agent hooks.
    pub fn edges_to(&self, node_id: NodeId) -> Result<Vec<cortex_core::Edge>> {
        let storage = open_transient_storage(&self.config)?;
        storage.edges_to(node_id).context("cortex edges_to")
    }

    /// Delete an edge by its ID.
    ///
    /// Opens a transient storage handle. Only call from short-lived
    /// CLI commands, not from agent hooks.
    pub fn delete_edge(&self, edge_id: cortex_core::EdgeId) -> Result<()> {
        let storage = open_transient_storage(&self.config)?;
        storage.delete_edge(edge_id).context("cortex delete_edge")
    }

    /// Atomically update the weight of an edge between two nodes.
    /// Takes a relation filter and a weight-update closure.
    /// Returns `(old_weight, new_weight)`.
    ///
    /// Opens a transient storage handle. Only call from short-lived
    /// CLI commands, not from agent hooks.
    pub fn update_edge_weight_atomic(
        &self,
        from: NodeId,
        to: NodeId,
        relation: &cortex_core::Relation,
        f: impl FnOnce(f32) -> f32,
    ) -> Result<(f32, f32)> {
        let storage = open_transient_storage(&self.config)?;
        storage
            .update_edge_weight_atomic(from, to, relation, f)
            .context("cortex update edge weight")
    }

    // ── Search ──

    /// Semantic vector search. Returns `(score, node)` pairs, highest first.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<(f32, Node)>> {
        self.inner.search(query, limit).context("cortex search")
    }

    /// Graph traversal from a node.
    pub fn traverse(&self, from: NodeId, depth: u32) -> Result<cortex_core::graph::Subgraph> {
        self.inner.traverse(from, depth).context("cortex traverse")
    }

    // ── Maintenance ──

    /// Run edge-decay. Call periodically to age out stale connections.
    /// Returns `(pruned_count, deleted_count)`.
    ///
    /// Opens a transient storage handle. Only call from short-lived
    /// CLI commands, not from agent hooks.
    pub fn run_decay(&self) -> Result<(u64, u64)> {
        let storage = Arc::new(open_transient_storage(&self.config)?);
        let engine = DecayEngine::new(
            storage,
            DecayConfig {
                daily_decay_rate: 0.01,
                prune_threshold: 0.1,
                delete_threshold: 0.05,
                importance_shield: 0.8,
                access_reinforcement_days: 7.0,
                exempt_manual: true,
            },
        );
        engine
            .apply_decay(chrono::Utc::now())
            .context("cortex decay")
    }

    /// Simple graph statistics.
    pub fn stats(&self) -> Result<CortexStats> {
        let nodes = self.inner.list_nodes(NodeFilter::new())?.len();
        // Count edges by traversing each node's neighbourhood.
        // This avoids opening a second RedbStorage handle (which
        // would fail with a lock conflict). We use `traverse` at
        // depth 1 to get each node's local edges, but dedup since
        // edges appear from both sides.
        let mut edge_count: usize = 0;
        let all_nodes = self.inner.list_nodes(NodeFilter::new())?;
        for node in &all_nodes {
            if let Ok(sg) = self.inner.traverse(node.id, 1) {
                // Each edge is counted from both endpoints; divide by 2.
                edge_count += sg.edges.len();
            }
        }
        // Each edge appears in two traversals (from + to), so halve.
        edge_count /= 2;
        Ok(CortexStats {
            nodes,
            edges: edge_count,
        })
    }

    /// Access the shared config.
    pub fn config(&self) -> &CortexConfig {
        &self.config
    }
}

/// Open a short-lived `RedbStorage` handle for edge management
/// and decay operations.
///
/// **CAUTION:** This acquires an exclusive file lock on `cortex.redb`.
/// It MUST only be called from short-lived CLI subcommands that exit
/// after the operation completes. Calling this from the agent loop
/// or TUI (which already holds the lock via `Cortex::open`) will fail.
fn open_transient_storage(config: &CortexConfig) -> Result<RedbStorage> {
    let db_path = resolve_db_path(config)?;
    RedbStorage::open(&db_path).context("open transient cortex storage")
}

#[derive(Debug, Clone)]
pub struct CortexStats {
    pub nodes: usize,
    pub edges: usize,
}

fn resolve_db_path(config: &CortexConfig) -> Result<PathBuf> {
    if let Some(ref p) = config.db_path {
        Ok(p.clone())
    } else {
        Ok(vulcan_home().join("cortex.redb"))
    }
}
