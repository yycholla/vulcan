//! Code-graph tools (YYC-50). Today: workspace symbol index + fast
//! lookup. Call-edges + impact-analysis are deferred to a follow-up
//! (need real LSP call hierarchy work); the schema reserves columns
//! so they can land incrementally.

use crate::code::graph::CodeGraph;
use crate::tools::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct IndexCodeGraphTool {
    graph: Arc<CodeGraph>,
}

impl IndexCodeGraphTool {
    pub fn new(graph: Arc<CodeGraph>) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl Tool for IndexCodeGraphTool {
    fn name(&self) -> &str {
        "index_code_graph"
    }
    fn description(&self) -> &str {
        "(Re)build the workspace symbol index. Walks the cwd respecting .gitignore, parses each supported source file via tree-sitter, persists symbols to ~/.vulcan/code_graph/. Run once per session (or after large file movements) — `find_symbol` reads from the cached index."
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    async fn call(&self, _params: Value, cancel: CancellationToken) -> Result<ToolResult> {
        let graph = self.graph.clone();
        let task = tokio::task::spawn_blocking(move || graph.reindex());
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(ToolResult::err("Cancelled")),
            r = task => r??,
        };
        let (files, symbols) = result;
        Ok(ToolResult::ok(format!(
            "Indexed {symbols} symbols across {files} files into {}",
            self.graph.workspace_root().display()
        )))
    }
}

#[derive(Clone)]
pub struct FindSymbolTool {
    graph: Arc<CodeGraph>,
}

impl FindSymbolTool {
    pub fn new(graph: Arc<CodeGraph>) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl Tool for FindSymbolTool {
    fn name(&self) -> &str {
        "find_symbol"
    }
    fn description(&self) -> &str {
        "Look up where a symbol is declared across the indexed workspace. Returns one row per declaration with file + line range. Run `index_code_graph` first to populate the index."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Exact symbol name (case-sensitive)" },
                "limit": { "type": "integer", "default": 25 }
            },
            "required": ["name"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let name = params["name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("name required"))?;
        let limit = params["limit"]
            .as_u64()
            .map(|n| n as usize)
            .unwrap_or(25);
        let graph = self.graph.clone();
        let name_owned = name.to_string();
        let rows =
            tokio::task::spawn_blocking(move || graph.find_by_name(&name_owned, limit)).await??;
        if rows.is_empty() {
            return Ok(ToolResult::ok(format!(
                "No declarations of '{name}' in the indexed workspace. \
                 Run `index_code_graph` if the index is stale."
            )));
        }
        let payload = json!({
            "name": name,
            "matches": rows,
        });
        Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
    }
}
