//! Handlers for the `cortex.*` method namespace.
//!
//! All operations route through the shared `CortexStore` held in
//! [`DaemonState`]. This keeps the redb file open once (daemon lifetime)
//! instead of cold-opening for every CLI invocation, eliminating both
//! the 1-2s embedding-model load and the exclusive-lock conflict between
//! CLI and TUI.

use serde_json::json;

use crate::daemon::protocol::{ProtocolError, Response};
use crate::daemon::state::DaemonState;
use crate::memory::cortex::CortexStore;

// ── Store ──

pub async fn store(
    state: &DaemonState,
    id: String,
    text: &str,
    importance: Option<f32>,
) -> Response {
    let Some(store) = state.cortex() else {
        return no_cortex(id);
    };
    let imp = importance.unwrap_or(0.5);
    let node = CortexStore::fact(text, imp);
    match store.store(node) {
        Ok(node_id) => Response::ok(id, json!({ "node_id": node_id.to_string() })),
        Err(e) => Response::error(
            id,
            ProtocolError {
                code: "CORTEX_STORE_FAILED".into(),
                message: format!("{e}"),
                retryable: true,
            },
        ),
    }
}

// ── Search ──

pub async fn search(state: &DaemonState, id: String, query: &str, limit: usize) -> Response {
    let Some(ref store) = state.cortex() else {
        return no_cortex(id);
    };
    match store.search(query, limit) {
        Ok(results) => {
            let hits: Vec<_> = results
                .into_iter()
                .map(|(score, node)| {
                    json!({
                        "node_id": node.id.to_string(),
                        "kind": node.kind.as_str(),
                        "title": node.data.title,
                        "body": node.data.body,
                        "score": score,
                        "created_at": node.created_at.to_rfc3339(),
                        "importance": node.importance,
                    })
                })
                .collect();
            Response::ok(id, json!({ "results": hits }))
        }
        Err(e) => Response::error(
            id,
            ProtocolError {
                code: "CORTEX_SEARCH_FAILED".into(),
                message: format!("{e}"),
                retryable: true,
            },
        ),
    }
}

// ── Stats ──

pub async fn stats(state: &DaemonState, id: String) -> Response {
    let Some(ref store) = state.cortex() else {
        return no_cortex(id);
    };
    match store.stats() {
        Ok(s) => {
            let db_path = store.config().db_path.clone();
            let db_size = db_path
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .map(|m| m.len())
                .unwrap_or(0);
            Response::ok(
                id,
                json!({
                    "nodes": s.nodes,
                    "edges": s.edges,
                    "db_size": db_size,
                }),
            )
        }
        Err(e) => Response::error(
            id,
            ProtocolError {
                code: "CORTEX_STATS_FAILED".into(),
                message: format!("{e}"),
                retryable: true,
            },
        ),
    }
}

// ── Recall ──

pub async fn recall(state: &DaemonState, id: String, limit: usize) -> Response {
    let Some(ref store) = state.cortex() else {
        return no_cortex(id);
    };
    let filter = cortex_core::NodeFilter::new().with_limit(limit.max(50));
    match store.list_nodes(filter) {
        Ok(nodes) => {
            let items: Vec<_> = nodes
                .into_iter()
                .map(|n| {
                    json!({
                        "node_id": n.id.to_string(),
                        "kind": n.kind.as_str(),
                        "title": n.data.title,
                        "body": n.data.body,
                        "created_at": n.created_at.to_rfc3339(),
                        "importance": n.importance,
                    })
                })
                .collect();
            Response::ok(id, json!({ "nodes": items }))
        }
        Err(e) => Response::error(
            id,
            ProtocolError {
                code: "CORTEX_RECALL_FAILED".into(),
                message: format!("{e}"),
                retryable: true,
            },
        ),
    }
}

// ── Seed ──

pub async fn seed(state: &DaemonState, id: String, sessions: usize) -> Response {
    let Some(ref store) = state.cortex() else {
        return no_cortex(id);
    };
    match crate::cli_cortex::seed_from_sessions_to(sessions, store).await {
        Ok(count) => Response::ok(id, json!({ "stored": count })),
        Err(e) => Response::error(
            id,
            ProtocolError {
                code: "CORTEX_SEED_FAILED".into(),
                message: format!("{e}"),
                retryable: true,
            },
        ),
    }
}

// ── Edge management ──

pub async fn edges_from(state: &DaemonState, id: String, node_id: &str) -> Response {
    let Some(ref store) = state.cortex() else {
        return no_cortex(id);
    };
    let nid = match parse_node_id(node_id, &id) {
        Ok(n) => n,
        Err(r) => return r,
    };
    match store.edges_from(nid) {
        Ok(edges) => Response::ok(id, json!({ "edges": serialize_edges(edges) })),
        Err(e) => Response::error(
            id,
            ProtocolError {
                code: "CORTEX_EDGES_FAILED".into(),
                message: format!("{e}"),
                retryable: true,
            },
        ),
    }
}

pub async fn edges_to(state: &DaemonState, id: String, node_id: &str) -> Response {
    let Some(ref store) = state.cortex() else {
        return no_cortex(id);
    };
    let nid = match parse_node_id(node_id, &id) {
        Ok(n) => n,
        Err(r) => return r,
    };
    match store.edges_to(nid) {
        Ok(edges) => Response::ok(id, json!({ "edges": serialize_edges(edges) })),
        Err(e) => Response::error(
            id,
            ProtocolError {
                code: "CORTEX_EDGES_FAILED".into(),
                message: format!("{e}"),
                retryable: true,
            },
        ),
    }
}

pub async fn delete_edge(state: &DaemonState, id: String, edge_id: &str) -> Response {
    let Some(ref store) = state.cortex() else {
        return no_cortex(id);
    };
    let eid = match cortex_core::EdgeId::parse_str(edge_id) {
        Ok(e) => e,
        Err(_) => {
            return Response::error(
                id,
                ProtocolError {
                    code: "INVALID_EDGE_ID".into(),
                    message: format!("invalid edge id: {edge_id}"),
                    retryable: false,
                },
            );
        }
    };
    match store.delete_edge(eid) {
        Ok(_) => Response::ok(id, json!({ "ok": true })),
        Err(e) => Response::error(
            id,
            ProtocolError {
                code: "CORTEX_DELETE_FAILED".into(),
                message: format!("{e}"),
                retryable: true,
            },
        ),
    }
}

// ── Decay ──

pub async fn run_decay(state: &DaemonState, id: String) -> Response {
    let Some(ref store) = state.cortex() else {
        return no_cortex(id);
    };
    match store.run_decay() {
        Ok((pruned, deleted)) => Response::ok(
            id,
            json!({
                "pruned": pruned,
                "deleted": deleted,
            }),
        ),
        Err(e) => Response::error(
            id,
            ProtocolError {
                code: "CORTEX_DECAY_FAILED".into(),
                message: format!("{e}"),
                retryable: true,
            },
        ),
    }
}

// ═══════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════

fn no_cortex(id: String) -> Response {
    Response::error(
        id,
        ProtocolError {
            code: "CORTEX_DISABLED".into(),
            message: "cortex is not enabled in this daemon".into(),
            retryable: false,
        },
    )
}

fn parse_node_id(node_id: &str, req_id: &str) -> Result<cortex_core::NodeId, Response> {
    cortex_core::NodeId::parse_str(node_id).map_err(|_| {
        Response::error(
            req_id.into(),
            ProtocolError {
                code: "INVALID_NODE_ID".into(),
                message: format!("invalid node id: {node_id}"),
                retryable: false,
            },
        )
    })
}

fn serialize_edges(edges: Vec<cortex_core::Edge>) -> Vec<serde_json::Value> {
    edges
        .into_iter()
        .map(|e| {
            json!({
                "id": e.id.to_string(),
                "from": e.from.to_string(),
                "to": e.to.to_string(),
                "relation": e.relation.as_str(),
                "weight": e.weight,
                "created_at": e.created_at.to_rfc3339(),
                "provenance": format!("{:?}", e.provenance),
            })
        })
        .collect()
}
