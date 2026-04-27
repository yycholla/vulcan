//! LSP-backed semantic code tools (YYC-46).
//!
//! Wraps `code::lsp::LspManager` so each tool gets the right server
//! (lazily spawned per language) for the file it was given. When no
//! server is available for the language (or the binary isn't on PATH),
//! the tool surfaces a clear error and the agent can fall back to the
//! tree-sitter tools (YYC-45).

use crate::code::Language;
use crate::code::lsp::{LspManager, diagnostics_for, find_references, goto_definition, hover};
use crate::tools::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

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
        let path = params["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path required"))?;
        let line = params["line"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("line required"))?;
        let character = params["character"].as_u64().unwrap_or(0);
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
        let path = params["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path required"))?;
        let line = params["line"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("line required"))?;
        let character = params["character"].as_u64().unwrap_or(0);
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
        let path = params["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path required"))?;
        let line = params["line"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("line required"))?;
        let character = params["character"].as_u64().unwrap_or(0);
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
        let path = params["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path required"))?;
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
