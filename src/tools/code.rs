//! Tree-sitter-backed structural code tools (YYC-45).
//!
//! - `code_outline`: symbol tree with line ranges. Token-cheap
//!   replacement for "read the whole file."
//! - `code_extract`: return just one named function/method/class body.
//! - `code_query`: run a tree-sitter S-expression query across files.
//!
//! All three reuse a per-language parser cache held by the registry to
//! avoid re-initializing parsers per call.

use crate::code::{Language, ParserCache};
use crate::tools::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tree_sitter::{Query, QueryCursor, StreamingIterator};

/// Cap output size so the LLM doesn't get megabyte responses on large
/// repos. The agent can narrow with paths or query refinements.
const MAX_OUTLINE_SYMBOLS: usize = 500;
const MAX_QUERY_HITS: usize = 200;

#[derive(Clone)]
pub struct CodeOutlineTool {
    cache: Arc<ParserCache>,
}

impl CodeOutlineTool {
    pub fn new(cache: Arc<ParserCache>) -> Self {
        Self { cache }
    }
}

#[async_trait]
impl Tool for CodeOutlineTool {
    fn name(&self) -> &str {
        "code_outline"
    }
    fn description(&self) -> &str {
        "Structural outline of a source file: top-level functions/types/etc with line ranges. JSON. Cheaper than read_file when the agent only needs the shape of a file. Use this instead of `grep '^fn\\|^pub'` via bash."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to source file" }
            },
            "required": ["path"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let path = params["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path required"))?;
        let pb = PathBuf::from(path);
        let lang = match Language::from_path(&pb) {
            Some(l) => l,
            None => {
                return Ok(ToolResult::err(format!(
                    "Unsupported file type for code_outline: {path}"
                )));
            }
        };
        let source = tokio::fs::read_to_string(path).await?;
        let symbols = outline(&self.cache, lang, &source)?;
        let payload = json!({
            "path": path,
            "language": lang.name(),
            "truncated": symbols.len() >= MAX_OUTLINE_SYMBOLS,
            "symbols": symbols,
        });
        Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
    }
}

#[derive(Clone)]
pub struct CodeExtractTool {
    cache: Arc<ParserCache>,
}

impl CodeExtractTool {
    pub fn new(cache: Arc<ParserCache>) -> Self {
        Self { cache }
    }
}

#[async_trait]
impl Tool for CodeExtractTool {
    fn name(&self) -> &str {
        "code_extract"
    }
    fn description(&self) -> &str {
        "Return the source body of a single named symbol (function / class / type). When the agent already knows what it wants and shouldn't pay for surrounding code. Use this instead of `awk '/fn name/,/^}/'` or `sed` ranges via bash."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to source file" },
                "symbol": { "type": "string", "description": "Symbol name (case-sensitive). First match wins." }
            },
            "required": ["path", "symbol"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let path = params["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path required"))?;
        let symbol = params["symbol"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("symbol required"))?;
        let pb = PathBuf::from(path);
        let lang = Language::from_path(&pb)
            .ok_or_else(|| anyhow::anyhow!("Unsupported file type for code_extract: {path}"))?;
        let source = tokio::fs::read_to_string(path).await?;
        let symbols = outline(&self.cache, lang, &source)?;
        let hit = symbols.into_iter().find(|s| s.name == symbol);
        match hit {
            None => Ok(ToolResult::err(format!(
                "Symbol '{symbol}' not found in {path}. Try `code_outline` to see available names."
            ))),
            Some(s) => {
                let snippet: String = source
                    .lines()
                    .skip(s.start_line.saturating_sub(1))
                    .take(s.end_line.saturating_sub(s.start_line.saturating_sub(1)))
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(ToolResult::ok(format!(
                    "{}:{}-{} ({})\n{snippet}",
                    path, s.start_line, s.end_line, s.kind
                )))
            }
        }
    }
}

#[derive(Clone)]
pub struct CodeQueryTool {
    cache: Arc<ParserCache>,
}

impl CodeQueryTool {
    pub fn new(cache: Arc<ParserCache>) -> Self {
        Self { cache }
    }
}

#[async_trait]
impl Tool for CodeQueryTool {
    fn name(&self) -> &str {
        "code_query"
    }
    fn description(&self) -> &str {
        "Run a tree-sitter S-expression query against one source file. Strictly more expressive than ripgrep for structural patterns: '(function_item name: (identifier) @name)' to find all Rust fn names. Use this instead of `grep` for structural code patterns via bash."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Source file to query" },
                "query": {
                    "type": "string",
                    "description": "Tree-sitter S-expression query. Captures (e.g. @name) are returned with their text + position."
                }
            },
            "required": ["path", "query"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let path = params["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path required"))?;
        let query_text = params["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("query required"))?;
        let pb = PathBuf::from(path);
        let lang = Language::from_path(&pb)
            .ok_or_else(|| anyhow::anyhow!("Unsupported file type for code_query: {path}"))?;
        let source = tokio::fs::read_to_string(path).await?;
        let hits = run_query(&self.cache, lang, &source, query_text)?;
        let truncated = hits.len() >= MAX_QUERY_HITS;
        let payload = json!({
            "path": path,
            "language": lang.name(),
            "truncated": truncated,
            "hits": hits,
        });
        Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OutlineSymbol {
    pub name: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
}

fn outline(cache: &ParserCache, lang: Language, source: &str) -> Result<Vec<OutlineSymbol>> {
    let query_text = lang.outline_query();
    if query_text.is_empty() {
        return Ok(Vec::new());
    }
    cache.with_parser(lang, |parser| {
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse source"))?;
        let query = Query::new(&lang.grammar_for_query(), query_text)
            .map_err(|e| anyhow::anyhow!("query compile: {e}"))?;
        let name_idx = query.capture_index_for_name("name");
        let mut cursor = QueryCursor::new();
        let mut iter = cursor.matches(&query, tree.root_node(), source.as_bytes());
        let mut out = Vec::new();
        while let Some(m) = iter.next() {
            // The kind capture is the last one (the @function / @struct
            // wrapping the whole node). The name capture is "name".
            let mut name = None;
            let mut kind_node = None;
            let mut kind_label = "symbol".to_string();
            for cap in m.captures.iter() {
                let cap_name = &query.capture_names()[cap.index as usize];
                if Some(cap.index) == name_idx {
                    name = Some(
                        cap.node
                            .utf8_text(source.as_bytes())
                            .unwrap_or("")
                            .to_string(),
                    );
                } else {
                    kind_node = Some(cap.node);
                    kind_label = (*cap_name).to_string();
                }
            }
            if let (Some(n), Some(node)) = (name, kind_node) {
                let start = node.start_position().row + 1;
                let end = node.end_position().row + 1;
                out.push(OutlineSymbol {
                    name: n,
                    kind: kind_label,
                    start_line: start,
                    end_line: end,
                });
                if out.len() >= MAX_OUTLINE_SYMBOLS {
                    break;
                }
            }
        }
        Ok(out)
    })?
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryHit {
    pub capture: String,
    pub text: String,
    pub start_line: usize,
    pub end_line: usize,
}

fn run_query(
    cache: &ParserCache,
    lang: Language,
    source: &str,
    query_text: &str,
) -> Result<Vec<QueryHit>> {
    cache.with_parser(lang, |parser| {
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter failed to parse source"))?;
        let query = Query::new(&lang.grammar_for_query(), query_text)
            .map_err(|e| anyhow::anyhow!("query compile: {e}"))?;
        let mut cursor = QueryCursor::new();
        let mut iter = cursor.matches(&query, tree.root_node(), source.as_bytes());
        let mut hits = Vec::new();
        while let Some(m) = iter.next() {
            for cap in m.captures.iter() {
                let cap_name = (*query.capture_names()[cap.index as usize]).to_string();
                let text = cap
                    .node
                    .utf8_text(source.as_bytes())
                    .unwrap_or("")
                    .to_string();
                hits.push(QueryHit {
                    capture: cap_name,
                    text,
                    start_line: cap.node.start_position().row + 1,
                    end_line: cap.node.end_position().row + 1,
                });
                if hits.len() >= MAX_QUERY_HITS {
                    return Ok(hits);
                }
            }
        }
        Ok(hits)
    })?
}

// Helper trait to expose grammar() for use in this module without
// re-implementing the grammar match. Defined here (vs `code/mod.rs`) so
// the code module's API stays minimal.
trait LanguageExt {
    fn grammar_for_query(&self) -> tree_sitter::Language;
}

impl LanguageExt for Language {
    fn grammar_for_query(&self) -> tree_sitter::Language {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            Language::Json => tree_sitter_json::LANGUAGE.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn outline_extracts_rust_top_level_items() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(
            &path,
            "fn alpha() {}\nstruct Beta { x: i32 }\nimpl Beta {}\n",
        )
        .unwrap();
        let cache = Arc::new(ParserCache::new());
        let tool = CodeOutlineTool::new(cache);
        let result = tool
            .call(
                json!({"path": path.to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.output);
        let payload: Value = serde_json::from_str(&result.output).unwrap();
        let names: Vec<String> = payload["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap_or("").to_string())
            .collect();
        assert!(names.contains(&"alpha".to_string()), "got {names:?}");
        assert!(names.contains(&"Beta".to_string()), "got {names:?}");
    }

    #[tokio::test]
    async fn extract_returns_just_the_named_function() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(
            &path,
            "fn alpha() { 1 }\n\nfn beta() {\n    let x = 2;\n    x + 3\n}\n",
        )
        .unwrap();
        let cache = Arc::new(ParserCache::new());
        let tool = CodeExtractTool::new(cache);
        let result = tool
            .call(
                json!({"path": path.to_string_lossy(), "symbol": "beta"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("fn beta()"), "got {}", result.output);
        assert!(result.output.contains("x + 3"));
        assert!(!result.output.contains("fn alpha"), "got {}", result.output);
    }

    #[tokio::test]
    async fn query_returns_capture_hits() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "fn one() {}\nfn two() {}\n").unwrap();
        let cache = Arc::new(ParserCache::new());
        let tool = CodeQueryTool::new(cache);
        let result = tool
            .call(
                json!({
                    "path": path.to_string_lossy(),
                    "query": "(function_item name: (identifier) @name)"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.output);
        let payload: Value = serde_json::from_str(&result.output).unwrap();
        let hits = payload["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0]["text"], "one");
        assert_eq!(hits[1]["text"], "two");
    }
}
