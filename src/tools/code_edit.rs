//! AST-aware structural edit tools (YYC-49).
//!
//! Replaces fuzzy-string `edit_file` for the cases where structural
//! anchors are stronger:
//!
//! - `replace_function_body`: locate a function/method by name via
//!   tree-sitter, splice in a new body. Idempotent — re-running with a
//!   renamed symbol fails loudly rather than corrupting unrelated code.
//! - `rename_symbol`: defers to LSP `textDocument/rename` for
//!   workspace-correct renames; surfaces the proposed edits without
//!   applying them in v1 (caller agent can read them and decide).
//!
//! `add_method` and `add_import` are deferred to follow-ups (need
//! per-language splice rules); the LLM still has `edit_file` as the
//! pragmatic fallback for those today.

use crate::code::Language;
use crate::code::lsp::LspManager;
use crate::tools::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tree_sitter::{Parser as TsParser, Query, QueryCursor, StreamingIterator};

/// Find the body node of a function/method named `symbol` in `source`.
/// Returns `(body_start_byte, body_end_byte)` so the caller can splice.
/// Tree-sitter's `body:` field gives us the brace-delimited block that
/// includes the leading `{` and trailing `}` — replacing that node
/// preserves the signature line untouched.
fn find_function_body_range(
    lang: Language,
    source: &str,
    symbol: &str,
) -> Result<Option<(usize, usize)>> {
    let mut parser = TsParser::new();
    let grammar: tree_sitter::Language = match lang {
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::Go => tree_sitter_go::LANGUAGE.into(),
        Language::Json => return Ok(None),
    };
    parser
        .set_language(&grammar)
        .map_err(|e| anyhow::anyhow!("set_language: {e}"))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("parse failed"))?;

    // Per-language query: capture the function name + its body block.
    let query_text = match lang {
        Language::Rust => "(function_item name: (identifier) @name body: (block) @body)",
        Language::Python => "(function_definition name: (identifier) @name body: (block) @body)",
        Language::TypeScript | Language::JavaScript => {
            "(function_declaration name: (identifier) @name body: (statement_block) @body)"
        }
        Language::Go => "(function_declaration name: (identifier) @name body: (block) @body)",
        Language::Json => return Ok(None),
    };
    let query = Query::new(&grammar, query_text).map_err(|e| anyhow::anyhow!("query: {e}"))?;
    let mut cursor = QueryCursor::new();
    let mut iter = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let name_idx = query.capture_index_for_name("name");
    let body_idx = query.capture_index_for_name("body");
    while let Some(m) = iter.next() {
        let mut name_text: Option<&str> = None;
        let mut body_range: Option<(usize, usize)> = None;
        for cap in m.captures {
            if Some(cap.index) == name_idx {
                name_text = cap.node.utf8_text(source.as_bytes()).ok();
            } else if Some(cap.index) == body_idx {
                body_range = Some((cap.node.start_byte(), cap.node.end_byte()));
            }
        }
        if let (Some(n), Some(range)) = (name_text, body_range)
            && n == symbol
        {
            return Ok(Some(range));
        }
    }
    Ok(None)
}

#[derive(Clone)]
pub struct ReplaceFunctionBodyTool;

#[async_trait]
impl Tool for ReplaceFunctionBodyTool {
    fn name(&self) -> &str {
        "replace_function_body"
    }
    fn description(&self) -> &str {
        "Replace just the body of a named function/method (the brace-delimited block) — idempotent and structural; fails loudly when the symbol is missing rather than corrupting unrelated code. `new_body` should include the surrounding `{ ... }` braces."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Source file" },
                "symbol": { "type": "string", "description": "Function/method name (case-sensitive). First match wins." },
                "new_body": {
                    "type": "string",
                    "description": "Full new body INCLUDING the outer braces, e.g. `{\\n    let x = 42;\\n    x + 1\\n}`"
                }
            },
            "required": ["path", "symbol", "new_body"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let path = params["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path required"))?;
        let symbol = params["symbol"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("symbol required"))?;
        let new_body = params["new_body"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("new_body required"))?;
        let pb = PathBuf::from(path);
        let lang = match Language::from_path(&pb) {
            Some(l) => l,
            None => {
                return Ok(ToolResult::err(format!(
                    "Unsupported file type for replace_function_body: {path}"
                )));
            }
        };
        let source = tokio::fs::read_to_string(path).await?;
        let range = match find_function_body_range(lang, &source, symbol)? {
            Some(r) => r,
            None => {
                return Ok(ToolResult::err(format!(
                    "Function '{symbol}' not found in {path}. Use `code_outline` to see available symbols."
                )));
            }
        };

        let mut new_source = String::with_capacity(source.len() + new_body.len());
        new_source.push_str(&source[..range.0]);
        new_source.push_str(new_body);
        new_source.push_str(&source[range.1..]);
        tokio::fs::write(path, &new_source).await?;
        Ok(ToolResult::ok(format!(
            "Replaced body of `{symbol}` in {path} ({} bytes → {} bytes)",
            range.1 - range.0,
            new_body.len()
        )))
    }
}

#[derive(Clone)]
pub struct RenameSymbolTool {
    manager: Arc<LspManager>,
}

impl RenameSymbolTool {
    pub fn new(manager: Arc<LspManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for RenameSymbolTool {
    fn name(&self) -> &str {
        "rename_symbol"
    }
    fn description(&self) -> &str {
        "Rename a symbol across the workspace via LSP `textDocument/rename`. Returns the proposed WorkspaceEdit as JSON; the agent must apply the resulting per-file changes (a workspace-applier is a separate follow-up). Requires an LSP server for the language."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "line": { "type": "integer", "description": "1-indexed source line" },
                "character": { "type": "integer", "description": "0-indexed column" },
                "new_name": { "type": "string" }
            },
            "required": ["path", "line", "character", "new_name"]
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
        let new_name = params["new_name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("new_name required"))?;
        let pb = PathBuf::from(path);
        let lang = match Language::from_path(&pb) {
            Some(l) => l,
            None => return Ok(ToolResult::err(format!("Unsupported file type: {path}"))),
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

        // didOpen so the server has the file contents indexed.
        let source = tokio::fs::read_to_string(path).await?;
        server.did_open(&pb, &source).await?;

        let line0 = (line as u32).saturating_sub(1);
        let request = json!({
            "textDocument": { "uri": format!("file://{}", absolute_path(&pb)?) },
            "position": { "line": line0, "character": character },
            "newName": new_name,
        });
        let resp: Value = server
            .request("textDocument/rename", request)
            .await
            .map_err(|e| anyhow::anyhow!("rename request failed: {e}"))?;
        Ok(ToolResult::ok(serde_json::to_string_pretty(&resp)?))
    }
}

fn absolute_path(p: &PathBuf) -> Result<String> {
    let abs = if p.is_absolute() {
        p.clone()
    } else {
        std::env::current_dir()?.join(p)
    };
    Ok(abs.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn replace_function_body_swaps_block_only() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "fn alpha() { 1 }\n\nfn beta() {\n    1\n}\n").unwrap();
        let tool = ReplaceFunctionBodyTool;
        let result = tool
            .call(
                json!({
                    "path": path.to_string_lossy(),
                    "symbol": "beta",
                    "new_body": "{\n    42\n}"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.output);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("fn alpha() { 1 }"), "got {after}");
        assert!(after.contains("fn beta() {\n    42\n}"), "got {after}");
        assert!(!after.contains("fn beta() {\n    1\n}"), "got {after}");
    }

    #[tokio::test]
    async fn replace_function_body_missing_symbol_errors_clearly() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "fn alpha() {}\n").unwrap();
        let result = ReplaceFunctionBodyTool
            .call(
                json!({
                    "path": path.to_string_lossy(),
                    "symbol": "ghost",
                    "new_body": "{}"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("ghost"), "got {}", result.output);
        assert!(
            result.output.contains("code_outline"),
            "should hint at code_outline: {}",
            result.output
        );
    }
}
