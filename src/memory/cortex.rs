//! Embedded graph memory for Vulcan, wrapping `cortex-memory-core`.
//!
//! Provides a `CortexStore` that owns the Cortex graph database (redb-backed)
//! and exposes core operations: store facts, semantic search, graph traversal,
//! and edge management.
//!
//! All public methods are `&self` — `Cortex` and its storage use interior
//! mutability internally, so callers can pass `Arc<CortexStore>` through
//! hooks without locking.

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
    /// Shared redb handle so lower-level engines can operate on the same db.
    storage: Arc<RedbStorage>,
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

        let storage = Arc::new(
            RedbStorage::open(&db_path).context("re-open redb for shared storage handle")?,
        );

        tracing::info!(
            "cortex memory ready at {} (model: {})",
            db_path.display(),
            config.embedding_model,
        );

        Ok(Arc::new(Self {
            inner,
            storage,
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

    /// List all edges originating from `node_id`.
    pub fn edges_from(&self, node_id: NodeId) -> Result<Vec<cortex_core::Edge>> {
        self.storage
            .edges_from(node_id)
            .context("cortex edges_from")
    }

    /// List all edges terminating at `node_id`.
    pub fn edges_to(&self, node_id: NodeId) -> Result<Vec<cortex_core::Edge>> {
        self.storage.edges_to(node_id).context("cortex edges_to")
    }

    /// Delete an edge by its ID.
    pub fn delete_edge(&self, edge_id: cortex_core::EdgeId) -> Result<()> {
        self.storage
            .delete_edge(edge_id)
            .context("cortex delete_edge")
    }

    /// Atomically update the weight of an edge between two nodes.
    /// Takes a relation filter and a weight-update closure.
    /// Returns `(old_weight, new_weight)`.
    pub fn update_edge_weight_atomic(
        &self,
        from: NodeId,
        to: NodeId,
        relation: &cortex_core::Relation,
        f: impl FnOnce(f32) -> f32,
    ) -> Result<(f32, f32)> {
        self.storage
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
    pub fn run_decay(&self) -> Result<(u64, u64)> {
        let engine = DecayEngine::new(
            self.storage.clone(),
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
        let edges = self
            .storage
            .list_nodes(NodeFilter::new())?
            .iter()
            .map(|n| {
                self.storage
                    .edges_from(n.id)
                    .map(|e: Vec<_>| e.len())
                    .unwrap_or(0)
            })
            .sum::<usize>();
        Ok(CortexStats { nodes, edges })
    }

    /// Access the shared config.
    pub fn config(&self) -> &CortexConfig {
        &self.config
    }
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
