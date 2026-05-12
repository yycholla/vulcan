//! Code-graph tools (YYC-50). Today: workspace symbol index + fast
//! lookup. Call-edges + impact-analysis are deferred to a follow-up
//! (need real LSP call hierarchy work); the schema reserves columns
//! so they can land incrementally.

use crate::code::graph::{CodeGraph, CodeGraphIndexReport, LspIndexStatus};
use crate::code::lsp::LspManager;
use crate::tools::{ReplaySafety, Tool, ToolResult, parse_tool_params};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn default_find_symbol_limit() -> u64 {
    25
}

#[derive(Deserialize)]
struct FindSymbolParams {
    name: String,
    #[serde(default = "default_find_symbol_limit")]
    limit: u64,
}

#[derive(Deserialize)]
struct SymbolGraphQueryParams {
    name: String,
    #[serde(default = "default_find_symbol_limit")]
    limit: u64,
}

fn default_impact_depth() -> u64 {
    2
}

#[derive(Deserialize)]
struct ImpactAnalysisParams {
    name: String,
    #[serde(default = "default_impact_depth")]
    max_depth: u64,
    #[serde(default = "default_find_symbol_limit")]
    limit: u64,
}

#[derive(Clone)]
pub struct IndexCodeGraphTool {
    graph: Arc<CodeGraph>,
    lsp: Option<Arc<LspManager>>,
}

impl IndexCodeGraphTool {
    pub fn new(graph: Arc<CodeGraph>, lsp: Option<Arc<LspManager>>) -> Self {
        Self { graph, lsp }
    }
}

#[async_trait]
impl Tool for IndexCodeGraphTool {
    fn name(&self) -> &str {
        "index_code_graph"
    }
    fn description(&self) -> &str {
        "(Re)build the workspace code graph. Walks the cwd respecting .gitignore, parses each supported source file via tree-sitter, persists symbols to ~/.vulcan/code_graph/, then best-effort harvests LSP call/type/implementation edges when a language server is available. Missing or incomplete LSP support is reported but does not fail symbol indexing."
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    async fn call(
        &self,
        _params: Value,
        cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let graph = self.graph.clone();
        let task = tokio::task::spawn_blocking(move || graph.reindex_with_edges(None));
        let mut report = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(ToolResult::err("Cancelled")),
            r = task => r??,
        };

        if let Some(lsp) = &self.lsp {
            let edge_report = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(ToolResult::err("Cancelled")),
                r = self.graph.harvest_lsp_edges(lsp) => r?,
            };
            merge_edge_report(&mut report, edge_report);
        }

        let payload = json!({
            "workspace": self.graph.workspace_root(),
            "files_indexed": report.files_indexed,
            "symbols_inserted": report.symbols_inserted,
            "edges_inserted": report.edges_inserted,
            "lsp_status": report.lsp_status,
            "lsp_errors": report.lsp_errors,
        });
        Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
    }
}

fn merge_edge_report(symbol_report: &mut CodeGraphIndexReport, edge_report: CodeGraphIndexReport) {
    symbol_report.edges_inserted = edge_report.edges_inserted;
    symbol_report.lsp_status = edge_report.lsp_status;
    symbol_report.lsp_errors = edge_report.lsp_errors;
    if symbol_report.lsp_status == LspIndexStatus::Unavailable && symbol_report.edges_inserted > 0 {
        symbol_report.lsp_status = LspIndexStatus::Partial;
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
    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::ReadOnly
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
    async fn call(
        &self,
        params: Value,
        _cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: FindSymbolParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let limit = p.limit as usize;
        let graph = self.graph.clone();
        let name_owned = p.name.clone();
        let rows =
            tokio::task::spawn_blocking(move || graph.find_by_name(&name_owned, limit)).await??;
        if rows.is_empty() {
            return Ok(ToolResult::ok(format!(
                "No declarations of '{}' in the indexed workspace. \
                 Run `index_code_graph` if the index is stale.",
                p.name
            )));
        }
        let payload = json!({
            "name": p.name,
            "matches": rows,
        });
        Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?).with_details(payload))
    }
}

#[derive(Clone)]
pub struct FindCallersTool {
    graph: Arc<CodeGraph>,
}

impl FindCallersTool {
    pub fn new(graph: Arc<CodeGraph>) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl Tool for FindCallersTool {
    fn name(&self) -> &str {
        "find_callers"
    }
    fn description(&self) -> &str {
        "Find symbols that call the named symbol from the persisted code graph. Returns source symbol, traversed call edges, limit, and truncation status. Run `index_code_graph` first."
    }
    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::ReadOnly
    }
    fn schema(&self) -> Value {
        symbol_query_schema("Exact callee symbol name (case-sensitive)")
    }
    async fn call(
        &self,
        params: Value,
        _cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: SymbolGraphQueryParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let graph = self.graph.clone();
        let name = p.name.clone();
        let limit = p.limit as usize;
        let payload =
            tokio::task::spawn_blocking(move || graph.find_callers(&name, limit)).await??;
        json_tool_result(payload)
    }
}

#[derive(Clone)]
pub struct FindCalleesTool {
    graph: Arc<CodeGraph>,
}

impl FindCalleesTool {
    pub fn new(graph: Arc<CodeGraph>) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl Tool for FindCalleesTool {
    fn name(&self) -> &str {
        "find_callees"
    }
    fn description(&self) -> &str {
        "Find symbols called by the named symbol from the persisted code graph. Returns source symbol, traversed call edges, limit, and truncation status. Run `index_code_graph` first."
    }
    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::ReadOnly
    }
    fn schema(&self) -> Value {
        symbol_query_schema("Exact caller symbol name (case-sensitive)")
    }
    async fn call(
        &self,
        params: Value,
        _cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: SymbolGraphQueryParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let graph = self.graph.clone();
        let name = p.name.clone();
        let limit = p.limit as usize;
        let payload =
            tokio::task::spawn_blocking(move || graph.find_callees(&name, limit)).await??;
        json_tool_result(payload)
    }
}

#[derive(Clone)]
pub struct TypeHierarchyTool {
    graph: Arc<CodeGraph>,
}

impl TypeHierarchyTool {
    pub fn new(graph: Arc<CodeGraph>) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl Tool for TypeHierarchyTool {
    fn name(&self) -> &str {
        "type_hierarchy"
    }
    fn description(&self) -> &str {
        "Find implementations plus subtype/supertype inheritance edges for a type/trait/interface from the persisted code graph. Returns traversed edge kinds, limit, and truncation status. Run `index_code_graph` first."
    }
    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::ReadOnly
    }
    fn schema(&self) -> Value {
        symbol_query_schema("Exact type, trait, or interface symbol name (case-sensitive)")
    }
    async fn call(
        &self,
        params: Value,
        _cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: SymbolGraphQueryParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let graph = self.graph.clone();
        let name = p.name.clone();
        let limit = p.limit as usize;
        let payload =
            tokio::task::spawn_blocking(move || graph.type_hierarchy(&name, limit)).await??;
        json_tool_result(payload)
    }
}

#[derive(Clone)]
pub struct GraphImpactAnalysisTool {
    graph: Arc<CodeGraph>,
}

impl GraphImpactAnalysisTool {
    pub fn new(graph: Arc<CodeGraph>) -> Self {
        Self { graph }
    }
}

#[async_trait]
impl Tool for GraphImpactAnalysisTool {
    fn name(&self) -> &str {
        "impact_analysis"
    }
    fn description(&self) -> &str {
        "Run bounded graph-backed impact analysis for a changed symbol by traversing reverse call edges. Returns impacted symbols, via edges, max_depth, limit, and truncation status. Run `index_code_graph` first."
    }
    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::ReadOnly
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Exact changed/source symbol name (case-sensitive)" },
                "max_depth": { "type": "integer", "default": default_impact_depth() },
                "limit": { "type": "integer", "default": default_find_symbol_limit() }
            },
            "required": ["name"]
        })
    }
    async fn call(
        &self,
        params: Value,
        _cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: ImpactAnalysisParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let graph = self.graph.clone();
        let name = p.name.clone();
        let max_depth = p.max_depth as usize;
        let limit = p.limit as usize;
        let payload =
            tokio::task::spawn_blocking(move || graph.impact_analysis(&name, max_depth, limit))
                .await??;
        json_tool_result(payload)
    }
}

fn symbol_query_schema(name_description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": { "type": "string", "description": name_description },
            "limit": { "type": "integer", "default": default_find_symbol_limit() }
        },
        "required": ["name"]
    })
}

fn json_tool_result<T: serde::Serialize>(payload: T) -> Result<ToolResult> {
    let value = serde_json::to_value(payload)?;
    Ok(ToolResult::ok(serde_json::to_string_pretty(&value)?).with_details(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::ParserCache;
    use crate::code::graph::{CodeGraphEdge, EdgeKind, EdgeProvider};
    use crate::tools::ReplaySafety;
    use tempfile::tempdir;

    #[tokio::test]
    async fn graph_query_tools_return_structured_json_and_are_read_only() {
        let graph = Arc::new(graph_with_edges(&[
            edge(EdgeKind::Call, "caller", "leaf"),
            edge(EdgeKind::Call, "leaf", "callee"),
            edge(EdgeKind::Implementation, "Impl", "Service"),
        ]));

        let callers = FindCallersTool::new(graph.clone());
        let result = callers
            .call(
                json!({"name": "leaf", "limit": 5}),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(callers.replay_safety(), ReplaySafety::ReadOnly);
        let payload: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(payload["source_symbol"], "leaf");
        assert_eq!(payload["edge_kind"], "Call");
        assert_eq!(payload["edges"][0]["source_name"], "caller");

        let callees = FindCalleesTool::new(graph.clone());
        assert_eq!(callees.replay_safety(), ReplaySafety::ReadOnly);
        let result = callees
            .call(
                json!({"name": "leaf", "limit": 5}),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        let payload: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(payload["edges"][0]["target_name"], "callee");

        let hierarchy = TypeHierarchyTool::new(graph.clone());
        assert_eq!(hierarchy.replay_safety(), ReplaySafety::ReadOnly);
        let result = hierarchy
            .call(
                json!({"name": "Service", "limit": 5}),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        let payload: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(payload["implementations"][0]["source_name"], "Impl");

        let impact = GraphImpactAnalysisTool::new(graph);
        assert_eq!(impact.replay_safety(), ReplaySafety::ReadOnly);
        let result = impact
            .call(
                json!({"name": "callee", "max_depth": 2, "limit": 5}),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        let payload: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(payload["source_symbol"], "callee");
        assert_eq!(payload["impacted_symbols"][0]["symbol"], "leaf");
    }

    fn graph_with_edges(edges: &[CodeGraphEdge]) -> CodeGraph {
        let dir = tempdir().unwrap().keep();
        std::fs::write(dir.join("a.rs"), "fn placeholder() {}\n").unwrap();
        let graph = CodeGraph::open(dir, Arc::new(ParserCache::new())).unwrap();
        graph.reindex().unwrap();
        graph.replace_all_edges(edges).unwrap();
        graph
    }

    fn edge(kind: EdgeKind, source: &str, target: &str) -> CodeGraphEdge {
        CodeGraphEdge {
            kind,
            source_file: "a.rs".into(),
            source_name: Some(source.into()),
            source_start_line: 1,
            source_start_character: 1,
            target_file: "a.rs".into(),
            target_name: Some(target.into()),
            target_start_line: 2,
            target_start_character: 1,
            provider: EdgeProvider::Lsp,
        }
    }
}
