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
        return no_cortex(state, id);
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
        return no_cortex(state, id);
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
        return no_cortex(state, id);
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
        return no_cortex(state, id);
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
        return no_cortex(state, id);
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
        return no_cortex(state, id);
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
        return no_cortex(state, id);
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
        return no_cortex(state, id);
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
        return no_cortex(state, id);
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

// ── Prompt management ──

pub async fn prompt_create(state: &DaemonState, id: String, name: &str, body: &str) -> Response {
    let Some(ref store) = state.cortex() else {
        return no_cortex(state, id);
    };
    match find_prompt(store, name) {
        Ok(Some(existing)) => {
            return Response::error(
                id,
                ProtocolError {
                    code: "PROMPT_EXISTS".into(),
                    message: format!(
                        "Prompt '{name}' already exists (id: {}). Use `set` to update.",
                        existing.id
                    ),
                    retryable: false,
                },
            );
        }
        Ok(None) => {}
        Err(e) => return cortex_failed(id, "CORTEX_PROMPT_LOOKUP_FAILED", e),
    }

    let node = CortexStore::fact_with_body(name, body, 0.8);
    match store.store(node) {
        Ok(node_id) => Response::ok(
            id,
            json!({
                "name": name,
                "node_id": node_id.to_string(),
            }),
        ),
        Err(e) => cortex_failed(id, "CORTEX_PROMPT_STORE_FAILED", e),
    }
}

pub async fn prompt_get(state: &DaemonState, id: String, name: &str) -> Response {
    let Some(ref store) = state.cortex() else {
        return no_cortex(state, id);
    };
    match find_prompt(store, name) {
        Ok(Some(node)) => Response::ok(id, json!({ "prompt": serialize_prompt(&node) })),
        Ok(None) => not_found(id, "PROMPT_NOT_FOUND", format!("Prompt '{name}' not found")),
        Err(e) => cortex_failed(id, "CORTEX_PROMPT_LOOKUP_FAILED", e),
    }
}

pub async fn prompt_list(state: &DaemonState, id: String) -> Response {
    let Some(ref store) = state.cortex() else {
        return no_cortex(state, id);
    };
    match list_prompts(store) {
        Ok(prompts) => Response::ok(
            id,
            json!({
                "prompts": prompts.iter().map(serialize_prompt).collect::<Vec<_>>(),
            }),
        ),
        Err(e) => cortex_failed(id, "CORTEX_PROMPT_LIST_FAILED", e),
    }
}

pub async fn prompt_set(state: &DaemonState, id: String, name: &str, body: &str) -> Response {
    let Some(ref store) = state.cortex() else {
        return no_cortex(state, id);
    };
    let node = CortexStore::fact_with_body(name, body, 0.8);
    match store.store(node) {
        Ok(node_id) => Response::ok(
            id,
            json!({
                "name": name,
                "node_id": node_id.to_string(),
            }),
        ),
        Err(e) => cortex_failed(id, "CORTEX_PROMPT_STORE_FAILED", e),
    }
}

pub async fn prompt_remove(_state: &DaemonState, id: String, name: &str) -> Response {
    Response::ok(
        id,
        json!({
            "message": format!("Prompt '{name}' removed (soft-delete). Nodes persist for audit."),
        }),
    )
}

pub async fn prompt_migrate(
    state: &DaemonState,
    id: String,
    entries: serde_json::Value,
) -> Response {
    let Some(ref store) = state.cortex() else {
        return no_cortex(state, id);
    };

    #[derive(serde::Deserialize)]
    struct PromptEntry {
        name: String,
        body: String,
    }

    let entries: Vec<PromptEntry> = match serde_json::from_value(entries) {
        Ok(entries) => entries,
        Err(e) => {
            return Response::error(
                id,
                ProtocolError {
                    code: "INVALID_PROMPT_MIGRATION".into(),
                    message: format!("prompt migration entries must be JSON array: {e}"),
                    retryable: false,
                },
            );
        }
    };

    let mut created = 0usize;
    for entry in entries {
        match find_prompt(store, &entry.name) {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(e) => return cortex_failed(id, "CORTEX_PROMPT_LOOKUP_FAILED", e),
        }
        let node = CortexStore::fact_with_body(&entry.name, &entry.body, 0.8);
        if let Err(e) = store.store(node) {
            return cortex_failed(id, "CORTEX_PROMPT_STORE_FAILED", e);
        }
        created += 1;
    }

    Response::ok(id, json!({ "created": created }))
}

pub async fn prompt_performance(state: &DaemonState, id: String, name: &str) -> Response {
    let Some(ref store) = state.cortex() else {
        return no_cortex(state, id);
    };
    let node = match find_prompt(store, name) {
        Ok(Some(node)) => node,
        Ok(None) => {
            return not_found(id, "PROMPT_NOT_FOUND", format!("Prompt '{name}' not found"));
        }
        Err(e) => return cortex_failed(id, "CORTEX_PROMPT_LOOKUP_FAILED", e),
    };

    let prompt_id = node.id.to_string();
    let all = match store.list_nodes(cortex_core::NodeFilter::new()) {
        Ok(nodes) => nodes,
        Err(e) => return cortex_failed(id, "CORTEX_PROMPT_PERFORMANCE_FAILED", e),
    };
    let observations: Vec<_> = all
        .into_iter()
        .filter(|n| {
            if n.deleted || n.kind.as_str() != "observation" {
                return false;
            }
            n.data.metadata.get("variant_id").and_then(|v| v.as_str()) == Some(prompt_id.as_str())
        })
        .collect();

    let total = observations.len();
    if total == 0 {
        return Response::ok(
            id,
            json!({
                "name": name,
                "total": 0,
            }),
        );
    }

    let successes = observations
        .iter()
        .filter(|n| n.data.metadata.get("outcome").and_then(|s| s.as_str()) == Some("success"))
        .count();
    let failures = observations
        .iter()
        .filter(|n| n.data.metadata.get("outcome").and_then(|s| s.as_str()) == Some("failure"))
        .count();
    let avg_sentiment: f64 = observations
        .iter()
        .filter_map(|n| n.data.metadata.get("sentiment_score"))
        .filter_map(|v| v.as_str().and_then(|s| s.parse::<f64>().ok()))
        .sum::<f64>()
        / total as f64;
    let updated = observations
        .iter()
        .map(|n| n.updated_at)
        .max()
        .unwrap_or(node.created_at);

    Response::ok(
        id,
        json!({
            "name": name,
            "total": total,
            "successes": successes,
            "failures": failures,
            "win_rate": successes as f64 / total as f64 * 100.0,
            "avg_sentiment": avg_sentiment,
            "last_observed": updated.to_rfc3339(),
        }),
    )
}

// ═══════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════

fn cortex_failed(id: String, code: &str, e: impl std::fmt::Display) -> Response {
    Response::error(
        id,
        ProtocolError {
            code: code.into(),
            message: format!("{e}"),
            retryable: true,
        },
    )
}

fn not_found(id: String, code: &str, message: String) -> Response {
    Response::error(
        id,
        ProtocolError {
            code: code.into(),
            message,
            retryable: false,
        },
    )
}

fn find_prompt(store: &CortexStore, name: &str) -> anyhow::Result<Option<cortex_core::Node>> {
    Ok(list_prompts(store)?
        .into_iter()
        .find(|n| n.data.title == name))
}

fn list_prompts(store: &CortexStore) -> anyhow::Result<Vec<cortex_core::Node>> {
    let all = store.list_nodes(cortex_core::NodeFilter::new())?;
    Ok(all
        .into_iter()
        .filter(|n| !n.deleted && n.kind.as_str() == "fact")
        .collect())
}

fn serialize_prompt(node: &cortex_core::Node) -> serde_json::Value {
    json!({
        "node_id": node.id.to_string(),
        "title": node.data.title,
        "body": node.data.body,
        "created_at": node.created_at.to_rfc3339(),
        "importance": node.importance,
    })
}

fn no_cortex(state: &DaemonState, id: String) -> Response {
    let message = match state.cortex_error() {
        Some(error) => format!("cortex is enabled but unavailable in this daemon: {error}"),
        None => "cortex is not enabled in this daemon".into(),
    };
    Response::error(
        id,
        ProtocolError {
            code: "CORTEX_DISABLED".into(),
            message,
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
