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
//! `CortexStore` therefore owns the daemon's one `Arc<RedbStorage>` directly
//! and builds the high-level search/traversal adapters around it. Edge admin,
//! stats, and decay all reuse that handle instead of opening a transient
//! second database.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use cortex_core::{
    Cortex, Edge, EdgeProvenance, FastEmbedService, GraphEngine, GraphEngineImpl, HnswIndex,
    LibraryConfig, Node, NodeFilter, NodeId, Relation,
    linker::{DecayConfig, DecayEngine},
    storage::{RedbStorage, Storage},
    vector::{EmbeddingService, VectorIndex, embedding_input},
};

use crate::config::{CortexConfig, vulcan_home};

/// Wrapper around the embedded Cortex graph memory engine.
pub struct CortexStore {
    storage: Arc<RedbStorage>,
    embedding: Arc<FastEmbedService>,
    index: Arc<RwLock<HnswIndex>>,
    graph_engine: Arc<GraphEngineImpl<RedbStorage>>,
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

        let storage = Arc::new(
            RedbStorage::open(&db_path)
                .with_context(|| format!("open cortex db at {}", db_path.display()))?,
        );
        let embedding = Arc::new(create_embedding_service(&lib_config.embedding_model)?);
        let mut idx = HnswIndex::new(embedding.dimension());
        let mut any_embeddings = false;
        for node in storage
            .list_nodes(NodeFilter::new())
            .context("load cortex nodes")?
        {
            if let Some(emb) = &node.embedding {
                idx.insert(node.id, emb).context("index cortex node")?;
                any_embeddings = true;
            }
        }
        if any_embeddings {
            idx.rebuild().context("rebuild cortex vector index")?;
        }
        let index = Arc::new(RwLock::new(idx));
        let graph_engine = Arc::new(GraphEngineImpl::new(Arc::clone(&storage)));

        tracing::info!(
            "cortex memory ready at {} (model: {})",
            db_path.display(),
            config.embedding_model,
        );

        Ok(Arc::new(Self {
            storage,
            embedding,
            index,
            graph_engine,
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
    pub fn store(&self, mut node: Node) -> Result<NodeId> {
        if node.embedding.is_none() {
            let text = embedding_input(&node);
            node.embedding = Some(self.embedding.embed(&text).context("cortex embed node")?);
        }
        let id = node.id;
        let embedding = node
            .embedding
            .clone()
            .context("cortex node embedding missing after generation")?;
        self.storage.put_node(&node).context("cortex store node")?;
        self.index
            .write()
            .map_err(|_| anyhow::anyhow!("cortex vector index lock poisoned"))?
            .insert(id, &embedding)
            .context("cortex index node")?;
        Ok(id)
    }

    /// Retrieve a node by ID.
    pub fn get_node(&self, id: NodeId) -> Result<Option<Node>> {
        self.storage.get_node(id).context("cortex get node")
    }

    /// List nodes matching a filter.
    pub fn list_nodes(&self, filter: NodeFilter) -> Result<Vec<Node>> {
        self.storage.list_nodes(filter).context("cortex list nodes")
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
        self.put_edge(edge)
    }

    /// Store a complete edge value. Used by admin paths that need
    /// explicit provenance or weight updates.
    pub fn put_edge(&self, edge: Edge) -> Result<()> {
        self.storage.put_edge(&edge).context("cortex create edge")?;
        self.graph_engine.invalidate_cache();
        Ok(())
    }

    // ── Edge management ──
    //
    // These methods reuse the daemon-owned storage handle. They must not open
    // `RedbStorage` again while the daemon is running because redb holds an
    // exclusive database lock.

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
            .context("cortex delete_edge")?;
        self.graph_engine.invalidate_cache();
        Ok(())
    }

    /// Atomically update the weight of an edge between two nodes.
    /// Takes a relation filter and a weight-update closure.
    /// Returns `(old_weight, new_weight)`.
    ///
    pub fn update_edge_weight_atomic(
        &self,
        from: NodeId,
        to: NodeId,
        relation: &cortex_core::Relation,
        f: impl FnOnce(f32) -> f32,
    ) -> Result<(f32, f32)> {
        let updated = self
            .storage
            .update_edge_weight_atomic(from, to, relation, f)
            .context("cortex update edge weight")?;
        self.graph_engine.invalidate_cache();
        Ok(updated)
    }

    // ── Search ──

    /// Semantic vector search. Returns `(score, node)` pairs, highest first.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<(f32, Node)>> {
        let query_emb = self.embedding.embed(query).context("cortex embed query")?;
        let hits = self
            .index
            .read()
            .map_err(|_| anyhow::anyhow!("cortex vector index lock poisoned"))?
            .search(&query_emb, limit, None)
            .context("cortex vector search")?;
        let mut out = Vec::new();
        for hit in hits {
            if let Some(node) = self
                .storage
                .get_node(hit.node_id)
                .context("cortex search get node")?
            {
                out.push((hit.score, node));
            }
        }
        Ok(out)
    }

    /// Graph traversal from a node.
    pub fn traverse(&self, from: NodeId, depth: u32) -> Result<cortex_core::graph::Subgraph> {
        self.graph_engine
            .neighborhood(from, depth)
            .context("cortex traverse")
    }

    // ── Maintenance ──

    /// Run edge-decay. Call periodically to age out stale connections.
    /// Returns `(pruned_count, deleted_count)`.
    pub fn run_decay(&self) -> Result<(u64, u64)> {
        let engine = DecayEngine::new(
            Arc::clone(&self.storage),
            DecayConfig {
                daily_decay_rate: 0.01,
                prune_threshold: 0.1,
                delete_threshold: 0.05,
                importance_shield: 0.8,
                access_reinforcement_days: 7.0,
                exempt_manual: true,
            },
        );
        let result = engine
            .apply_decay(chrono::Utc::now())
            .context("cortex decay")?;
        self.graph_engine.invalidate_cache();
        Ok(result)
    }

    /// Simple graph statistics.
    pub fn stats(&self) -> Result<CortexStats> {
        let stats = self.storage.stats().context("cortex storage stats")?;
        Ok(CortexStats {
            nodes: stats.node_count as usize,
            edges: stats.edge_count as usize,
        })
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

fn create_embedding_service(model: &str) -> Result<FastEmbedService> {
    use fastembed::EmbeddingModel;
    match model {
        "BAAI/bge-base-en-v1.5" => FastEmbedService::with_model(EmbeddingModel::BGEBaseENV15),
        "BAAI/bge-large-en-v1.5" => FastEmbedService::with_model(EmbeddingModel::BGELargeENV15),
        _ => FastEmbedService::new(),
    }
    .context("initialize cortex embedding model")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config() -> (tempfile::TempDir, CortexConfig) {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = CortexConfig {
            enabled: true,
            db_path: Some(dir.path().join("cortex.redb")),
            ..CortexConfig::default()
        };
        (dir, config)
    }

    #[test]
    fn edge_admin_operations_use_open_store_handle() {
        let (_dir, config) = temp_config();
        let store = CortexStore::try_open(&config).expect("open cortex");
        let from = store
            .store(CortexStore::fact("source node", 0.5))
            .expect("store source");
        let to = store
            .store(CortexStore::fact("target node", 0.5))
            .expect("store target");

        store
            .create_edge(from, to, "supports", 0.75)
            .expect("create edge");

        let outgoing = store.edges_from(from).expect("edges_from");
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].to, to);

        let incoming = store.edges_to(to).expect("edges_to");
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].from, from);

        let edge_id = outgoing[0].id;
        store.delete_edge(edge_id).expect("delete edge");
        assert!(
            store
                .edges_from(from)
                .expect("edges after delete")
                .is_empty()
        );
    }

    #[test]
    fn stats_and_decay_use_open_store_handle() {
        let (_dir, config) = temp_config();
        let store = CortexStore::try_open(&config).expect("open cortex");
        let from = store
            .store(CortexStore::fact("source node", 0.5))
            .expect("store source");
        let to = store
            .store(CortexStore::fact("target node", 0.5))
            .expect("store target");
        store
            .create_edge(from, to, "supports", 0.75)
            .expect("create edge");

        let stats = store.stats().expect("stats");
        assert_eq!(stats.nodes, 2);
        assert_eq!(stats.edges, 1);

        let (_pruned, _deleted) = store.run_decay().expect("decay");
    }

    #[test]
    fn search_and_traverse_still_use_shared_store() {
        let (_dir, config) = temp_config();
        let store = CortexStore::try_open(&config).expect("open cortex");
        let from = store
            .store(CortexStore::fact_with_body(
                "rust daemon",
                "daemon owns runtime resources",
                0.7,
            ))
            .expect("store source");
        let to = store
            .store(CortexStore::fact_with_body(
                "runtime pool",
                "shared storage and cortex graph",
                0.7,
            ))
            .expect("store target");
        store
            .create_edge(from, to, "supports", 0.75)
            .expect("create edge");

        let hits = store.search("runtime resources", 5).expect("search");
        assert!(
            hits.iter()
                .any(|(_, node)| node.id == from || node.id == to),
            "search should read nodes indexed through shared store"
        );

        let graph = store.traverse(from, 1).expect("traverse");
        assert!(
            graph
                .edges
                .iter()
                .any(|edge| edge.from == from && edge.to == to),
            "traverse should read edges through shared store"
        );
    }
}
