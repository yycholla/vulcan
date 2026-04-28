//! LSP-backed semantic code tools (YYC-46).
//!
//! Wraps `code::lsp::LspManager` so each tool gets the right server
//! (lazily spawned per language) for the file it was given. When no
//! server is available for the language (or the binary isn't on PATH),
//! the tool surfaces a clear error and the agent can fall back to the
//! tree-sitter tools (YYC-45).

use crate::code::Language;
use crate::code::lsp::{
    LspManager, call_hierarchy_incoming, call_hierarchy_outgoing, code_action, diagnostics_for,
    find_references, goto_definition, hover, implementation, prepare_call_hierarchy,
    type_definition, workspace_symbol,
};
use crate::tools::{Tool, ToolResult, parse_tool_params};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
struct LspPositionParams {
    path: String,
    line: u64,
    #[serde(default)]
    character: u64,
}

#[derive(Deserialize)]
struct CodeActionParams {
    path: String,
    start_line: u64,
    #[serde(default)]
    end_line: Option<u64>,
    #[serde(default)]
    start_character: u64,
    #[serde(default)]
    end_character: u64,
}

#[derive(Deserialize)]
struct CallHierarchyParams {
    path: String,
    line: u64,
    #[serde(default)]
    character: u64,
    direction: String,
}

#[derive(Deserialize)]
struct WorkspaceSymbolParams {
    query: String,
    language: String,
}

#[derive(Deserialize)]
struct DiagnosticsParams {
    path: String,
}

/// Resolve `path` to the language for which we'd talk to an LSP. Used
/// by every tool below to fail fast on unsupported file types.
fn lang_for(path: &str) -> Result<Language> {
    let pb = PathBuf::from(path);
    Language::from_path(&pb)
        .ok_or_else(|| anyhow::anyhow!("Unsupported file type for LSP tools: {path}"))
}

#[derive(Clone)]
pub struct GotoDefinitionTool {
    manager: Arc<LspManager>,
}

impl GotoDefinitionTool {
    pub fn new(manager: Arc<LspManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for GotoDefinitionTool {
    fn name(&self) -> &str {
        "goto_definition"
    }
    fn description(&self) -> &str {
        "Resolve where a symbol at file:line:col is defined. Returns one or more locations as JSON. Falls back gracefully when no LSP server is installed for the language."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "line": { "type": "integer", "description": "1-indexed source line" },
                "character": { "type": "integer", "description": "0-indexed column" }
            },
            "required": ["path", "line", "character"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let p: LspPositionParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let path = p.path.as_str();
        let line = p.line;
        let character = p.character;
        let lang = match lang_for(path) {
            Ok(l) => l,
            Err(e) => return Ok(ToolResult::err(e.to_string())),
        };
        let server = match self.manager.server(lang).await {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolResult::err(format!(
                    "LSP unavailable for {}: {e}. Try the tree-sitter tools as a fallback.",
                    lang.name()
                )));
            }
        };
        let pb = PathBuf::from(path);
        // LSP positions are 0-indexed; translate the more agent-friendly
        // 1-indexed line input.
        let line0 = (line as u32).saturating_sub(1);
        let resp = match goto_definition(&server, &pb, line0, character as u32).await {
            Ok(r) => r,
            Err(e) => return Ok(ToolResult::err(format!("{e}"))),
        };
        let payload = json!({ "locations": resp.unwrap_or_default() });
        Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
    }
}

#[derive(Clone)]
pub struct FindReferencesTool {
    manager: Arc<LspManager>,
}

impl FindReferencesTool {
    pub fn new(manager: Arc<LspManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for FindReferencesTool {
    fn name(&self) -> &str {
        "find_references"
    }
    fn description(&self) -> &str {
        "List all references to the symbol at file:line:col across the workspace. Includes the declaration."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "line": { "type": "integer", "description": "1-indexed source line" },
                "character": { "type": "integer", "description": "0-indexed column" }
            },
            "required": ["path", "line", "character"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let p: LspPositionParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let path = p.path.as_str();
        let line = p.line;
        let character = p.character;
        let lang = match lang_for(path) {
            Ok(l) => l,
            Err(e) => return Ok(ToolResult::err(e.to_string())),
        };
        let server = match self.manager.server(lang).await {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolResult::err(format!(
                    "LSP unavailable for {}: {e}",
                    lang.name()
                )));
            }
        };
        let pb = PathBuf::from(path);
        let line0 = (line as u32).saturating_sub(1);
        let locs = match find_references(&server, &pb, line0, character as u32).await {
            Ok(r) => r.unwrap_or_default(),
            Err(e) => return Ok(ToolResult::err(format!("{e}"))),
        };
        let payload = json!({ "references": locs });
        Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
    }
}

#[derive(Clone)]
pub struct HoverTool {
    manager: Arc<LspManager>,
}

impl HoverTool {
    pub fn new(manager: Arc<LspManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for HoverTool {
    fn name(&self) -> &str {
        "hover"
    }
    fn description(&self) -> &str {
        "Show docs + type info for the symbol at file:line:col, as the language server reports it."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "line": { "type": "integer" },
                "character": { "type": "integer" }
            },
            "required": ["path", "line", "character"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let p: LspPositionParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let path = p.path.as_str();
        let line = p.line;
        let character = p.character;
        let lang = match lang_for(path) {
            Ok(l) => l,
            Err(e) => return Ok(ToolResult::err(e.to_string())),
        };
        let server = match self.manager.server(lang).await {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolResult::err(format!(
                    "LSP unavailable for {}: {e}",
                    lang.name()
                )));
            }
        };
        let pb = PathBuf::from(path);
        let line0 = (line as u32).saturating_sub(1);
        let resp = match hover(&server, &pb, line0, character as u32).await {
            Ok(r) => r,
            Err(e) => return Ok(ToolResult::err(format!("{e}"))),
        };
        let payload = json!({ "hover": resp });
        Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
    }
}

#[derive(Clone)]
pub struct TypeDefinitionTool {
    manager: Arc<LspManager>,
}

impl TypeDefinitionTool {
    pub fn new(manager: Arc<LspManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for TypeDefinitionTool {
    fn name(&self) -> &str {
        "type_definition"
    }
    fn description(&self) -> &str {
        "For an expression at file:line:col, jump to where its TYPE is declared. Different from goto_definition: that returns the symbol's binding site; this returns the declaration of the type. JSON locations."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "line": { "type": "integer", "description": "1-indexed source line" },
                "character": { "type": "integer", "description": "0-indexed column" }
            },
            "required": ["path", "line", "character"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let p: LspPositionParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let path = p.path.as_str();
        let line = p.line;
        let character = p.character;
        let lang = match lang_for(path) {
            Ok(l) => l,
            Err(e) => return Ok(ToolResult::err(e.to_string())),
        };
        let server = match self.manager.server(lang).await {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolResult::err(format!(
                    "LSP unavailable for {}: {e}",
                    lang.name()
                )));
            }
        };
        let pb = PathBuf::from(path);
        let line0 = (line as u32).saturating_sub(1);
        let resp = match type_definition(&server, &pb, line0, character as u32).await {
            Ok(r) => r,
            Err(e) => return Ok(ToolResult::err(format!("{e}"))),
        };
        let payload = json!({ "locations": resp.unwrap_or_default() });
        Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
    }
}

#[derive(Clone)]
pub struct ImplementationTool {
    manager: Arc<LspManager>,
}

impl ImplementationTool {
    pub fn new(manager: Arc<LspManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for ImplementationTool {
    fn name(&self) -> &str {
        "implementation"
    }
    fn description(&self) -> &str {
        "For a trait or interface at file:line:col, list every implementation site as JSON locations. In Rust this finds `impl Trait for Type` blocks; in Go/TS, the implementing types of an interface."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "line": { "type": "integer", "description": "1-indexed source line" },
                "character": { "type": "integer", "description": "0-indexed column" }
            },
            "required": ["path", "line", "character"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let p: LspPositionParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let path = p.path.as_str();
        let line = p.line;
        let character = p.character;
        let lang = match lang_for(path) {
            Ok(l) => l,
            Err(e) => return Ok(ToolResult::err(e.to_string())),
        };
        let server = match self.manager.server(lang).await {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolResult::err(format!(
                    "LSP unavailable for {}: {e}",
                    lang.name()
                )));
            }
        };
        let pb = PathBuf::from(path);
        let line0 = (line as u32).saturating_sub(1);
        let locs = match implementation(&server, &pb, line0, character as u32).await {
            Ok(r) => r.unwrap_or_default(),
            Err(e) => return Ok(ToolResult::err(format!("{e}"))),
        };
        let payload = json!({ "implementations": locs });
        Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
    }
}

#[derive(Clone)]
pub struct CodeActionTool {
    manager: Arc<LspManager>,
}

impl CodeActionTool {
    pub fn new(manager: Arc<LspManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for CodeActionTool {
    fn name(&self) -> &str {
        "code_action"
    }
    fn description(&self) -> &str {
        "List the LSP code actions (fix-its + refactors) available for a line range. Returns each action's title, kind, whether it's marked preferred by the server, and whether it carries an edit. This tool is read-only; applying the edit is a separate step."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "start_line": {
                    "type": "integer",
                    "description": "1-indexed source line where the range starts"
                },
                "end_line": {
                    "type": "integer",
                    "description": "1-indexed source line where the range ends (inclusive). Defaults to start_line."
                },
                "start_character": {
                    "type": "integer",
                    "description": "0-indexed column at start. Default 0."
                },
                "end_character": {
                    "type": "integer",
                    "description": "0-indexed column at end. Default 0; the server treats line-only ranges as the whole line."
                }
            },
            "required": ["path", "start_line"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        use lsp_types::Position as LspPosition;
        let p: CodeActionParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let path = p.path.as_str();
        let start_line = p.start_line;
        let end_line = p.end_line.unwrap_or(start_line);
        let start_char = p.start_character as u32;
        let end_char = p.end_character as u32;
        let lang = match lang_for(path) {
            Ok(l) => l,
            Err(e) => return Ok(ToolResult::err(e.to_string())),
        };
        let server = match self.manager.server(lang).await {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolResult::err(format!(
                    "LSP unavailable for {}: {e}",
                    lang.name()
                )));
            }
        };
        let pb = PathBuf::from(path);
        let start = LspPosition {
            line: (start_line as u32).saturating_sub(1),
            character: start_char,
        };
        let end = LspPosition {
            line: (end_line as u32).saturating_sub(1),
            character: end_char,
        };
        // Pull current cached diagnostics so the server can offer
        // diagnostic-keyed fixes (e.g. rust-analyzer's "import this
        // name"). Empty diagnostics are valid; the server still
        // returns refactors that don't depend on errors.
        let diags = server.cached_diagnostics(&pb).await;
        let actions = match code_action(&server, &pb, start, end, diags).await {
            Ok(a) => a,
            Err(e) => return Ok(ToolResult::err(format!("{e}"))),
        };
        // Project the LSP shape into a flat agent-friendly list.
        let hits: Vec<Value> = actions
            .into_iter()
            .map(|a| match a {
                lsp_types::CodeActionOrCommand::Command(cmd) => json!({
                    "title": cmd.title,
                    "kind": "command",
                    "is_preferred": false,
                    "has_edit": false,
                    "command": cmd.command,
                }),
                lsp_types::CodeActionOrCommand::CodeAction(action) => json!({
                    "title": action.title,
                    "kind": action.kind.map(|k| k.as_str().to_string()),
                    "is_preferred": action.is_preferred.unwrap_or(false),
                    "has_edit": action.edit.is_some(),
                    "command": action.command.map(|c| c.command),
                }),
            })
            .collect();
        let payload = json!({
            "path": path,
            "count": hits.len(),
            "actions": hits,
        });
        Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
    }
}

#[derive(Clone)]
pub struct CallHierarchyTool {
    manager: Arc<LspManager>,
}

impl CallHierarchyTool {
    pub fn new(manager: Arc<LspManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for CallHierarchyTool {
    fn name(&self) -> &str {
        "call_hierarchy"
    }
    fn description(&self) -> &str {
        "Find functions that call (`incoming`) or are called by (`outgoing`) the symbol at file:line:col. Stricter than find_references — type uses and imports don't show up here. Returns each call's name + file + line + container."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "line": { "type": "integer", "description": "1-indexed source line" },
                "character": { "type": "integer", "description": "0-indexed column" },
                "direction": {
                    "type": "string",
                    "enum": ["incoming", "outgoing"],
                    "description": "`incoming` lists callers; `outgoing` lists callees."
                }
            },
            "required": ["path", "line", "character", "direction"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let p: CallHierarchyParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let path = p.path.as_str();
        let line = p.line;
        let character = p.character;
        let direction = p.direction.as_str();
        if direction != "incoming" && direction != "outgoing" {
            return Ok(ToolResult::err(format!(
                "direction must be \"incoming\" or \"outgoing\"; got `{direction}`",
            )));
        }
        let lang = match lang_for(path) {
            Ok(l) => l,
            Err(e) => return Ok(ToolResult::err(e.to_string())),
        };
        let server = match self.manager.server(lang).await {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolResult::err(format!(
                    "LSP unavailable for {}: {e}",
                    lang.name()
                )));
            }
        };
        let pb = PathBuf::from(path);
        let line0 = (line as u32).saturating_sub(1);
        let items = match prepare_call_hierarchy(&server, &pb, line0, character as u32).await {
            Ok(items) => items,
            Err(e) => return Ok(ToolResult::err(format!("{e}"))),
        };
        if items.is_empty() {
            let payload = json!({
                "direction": direction,
                "calls": [],
                "note": "No callable symbol at this position.",
            });
            return Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?));
        }
        // The spec returns a `Vec` because some servers resolve a
        // position to multiple callable items (overloads). Expand
        // each into its own incoming/outgoing query and concatenate.
        let mut hits: Vec<Value> = Vec::new();
        for item in items {
            if direction == "incoming" {
                let calls = match call_hierarchy_incoming(&server, item).await {
                    Ok(c) => c,
                    Err(e) => return Ok(ToolResult::err(format!("{e}"))),
                };
                for call in calls {
                    let from = call.from;
                    hits.push(json!({
                        "name": from.name,
                        "kind": format!("{:?}", from.kind),
                        "container": from.detail,
                        "file": from.uri.path().as_str(),
                        "line": from.range.start.line + 1,
                        "call_sites": call.from_ranges.len(),
                    }));
                }
            } else {
                let calls = match call_hierarchy_outgoing(&server, item).await {
                    Ok(c) => c,
                    Err(e) => return Ok(ToolResult::err(format!("{e}"))),
                };
                for call in calls {
                    let to = call.to;
                    hits.push(json!({
                        "name": to.name,
                        "kind": format!("{:?}", to.kind),
                        "container": to.detail,
                        "file": to.uri.path().as_str(),
                        "line": to.range.start.line + 1,
                        "call_sites": call.from_ranges.len(),
                    }));
                }
            }
        }
        let payload = json!({
            "direction": direction,
            "count": hits.len(),
            "calls": hits,
        });
        Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
    }
}

#[derive(Clone)]
pub struct WorkspaceSymbolTool {
    manager: Arc<LspManager>,
}

impl WorkspaceSymbolTool {
    pub fn new(manager: Arc<LspManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for WorkspaceSymbolTool {
    fn name(&self) -> &str {
        "workspace_symbol"
    }
    fn description(&self) -> &str {
        "Search for symbols (functions, types, modules) by name across the workspace via LSP. Returns structured hits with file + line + container so the agent can find a definition without knowing the path. Falls back to a clear error when no LSP server is running for the language."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Substring or fuzzy match against symbol names. Empty string lists everything (server-defined order)."
                },
                "language": {
                    "type": "string",
                    "description": "Which language's LSP to query. Required because workspace_symbol is a per-server search; e.g. \"rust\", \"typescript\", \"python\", \"go\"."
                }
            },
            "required": ["query", "language"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let p: WorkspaceSymbolParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let query = p.query.as_str();
        let lang_name = p.language.as_str();
        let lang = match Language::from_name(lang_name) {
            Some(l) => l,
            None => {
                return Ok(ToolResult::err(format!(
                    "Unknown language `{lang_name}`. Supported: rust, typescript, javascript, python, go.",
                )));
            }
        };
        let server = match self.manager.server(lang).await {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolResult::err(format!(
                    "LSP unavailable for {}: {e}",
                    lang.name(),
                )));
            }
        };
        let symbols = match workspace_symbol(&server, query).await {
            Ok(r) => r.unwrap_or_default(),
            Err(e) => return Ok(ToolResult::err(format!("{e}"))),
        };
        // Project the LSP shape into a leaner JSON the agent can read
        // without learning lsp_types: name, kind (number → label),
        // container, file, line (1-indexed for parity with the
        // other tools).
        let hits: Vec<Value> = symbols
            .into_iter()
            .map(|s| {
                let path = s.location.uri.path().as_str().to_string();
                let line = s.location.range.start.line + 1;
                json!({
                    "name": s.name,
                    "kind": format!("{:?}", s.kind),
                    "container": s.container_name,
                    "file": path,
                    "line": line,
                })
            })
            .collect();
        let payload = json!({
            "query": query,
            "language": lang.name(),
            "count": hits.len(),
            "hits": hits,
        });
        Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
    }
}

#[cfg(test)]
mod workspace_symbol_tests {
    use super::*;
    use std::path::PathBuf;

    fn tool() -> WorkspaceSymbolTool {
        WorkspaceSymbolTool::new(Arc::new(LspManager::new(PathBuf::from("."))))
    }

    // YYC-201: missing query surfaces as ToolResult::err (YYC-263 typed params).
    #[tokio::test]
    async fn workspace_symbol_requires_query() {
        let t = tool();
        let result = t
            .call(json!({"language": "rust"}), CancellationToken::new())
            .await
            .expect("call ok");
        assert!(result.is_error);
        assert!(result.output.contains("tool params failed to validate"));
    }

    // YYC-201: missing language surfaces as ToolResult::err (YYC-263 typed params).
    #[tokio::test]
    async fn workspace_symbol_requires_language() {
        let t = tool();
        let result = t
            .call(json!({"query": "foo"}), CancellationToken::new())
            .await
            .expect("call ok");
        assert!(result.is_error);
        assert!(result.output.contains("tool params failed to validate"));
    }

    // YYC-201: unknown language → structured ToolResult error,
    // not a panic. Lists the supported set so the agent can retry.
    #[tokio::test]
    async fn workspace_symbol_unknown_language_returns_tool_error() {
        let t = tool();
        let result = t
            .call(
                json!({"query": "foo", "language": "klingon"}),
                CancellationToken::new(),
            )
            .await
            .expect("call ok");
        assert!(result.is_error);
        assert!(result.output.contains("Unknown language"));
        assert!(result.output.contains("rust"));
    }
}

#[derive(Clone)]
pub struct DiagnosticsTool {
    manager: Arc<LspManager>,
}

impl DiagnosticsTool {
    pub fn new(manager: Arc<LspManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for DiagnosticsTool {
    fn name(&self) -> &str {
        "diagnostics"
    }
    fn description(&self) -> &str {
        "Current LSP diagnostics for a file (errors, warnings, hints). The auto-diagnostics hook (YYC-51) runs this after edits, but the agent can call it manually too."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            },
            "required": ["path"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let p: DiagnosticsParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let path = p.path.as_str();
        let lang = match lang_for(path) {
            Ok(l) => l,
            Err(e) => return Ok(ToolResult::err(e.to_string())),
        };
        let server = match self.manager.server(lang).await {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolResult::err(format!(
                    "LSP unavailable for {}: {e}",
                    lang.name()
                )));
            }
        };
        let pb = PathBuf::from(path);
        let diags = diagnostics_for(&server, &pb).await?;
        let payload = json!({
            "path": path,
            "count": diags.len(),
            "diagnostics": diags,
        });
        Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
    }
}
