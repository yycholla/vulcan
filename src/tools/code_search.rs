//! Embedding-backed semantic search tools (YYC-48).
//!
//! `index_code_embeddings` runs the indexer (long-running on first
//! call); `code_search_semantic` answers ranked queries.

use crate::code::embed::EmbeddingIndex;
use crate::tools::{Tool, ToolResult, parse_tool_params};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn default_semantic_top_k() -> u64 {
    8
}

#[derive(Deserialize)]
struct CodeSearchSemanticParams {
    query: String,
    #[serde(default = "default_semantic_top_k")]
    top_k: u64,
}

#[derive(Clone)]
pub struct IndexEmbeddingsTool {
    index: Arc<EmbeddingIndex>,
}

impl IndexEmbeddingsTool {
    pub fn new(index: Arc<EmbeddingIndex>) -> Self {
        Self { index }
    }
}

#[async_trait]
impl Tool for IndexEmbeddingsTool {
    fn name(&self) -> &str {
        "index_code_embeddings"
    }
    fn description(&self) -> &str {
        "(Re)build the semantic embedding index of the workspace. Walks cwd, chunks each supported source file by top-level symbol via tree-sitter, embeds each chunk, persists to ~/.vulcan/embeddings/. Long-running on first call. Requires [embeddings] enabled in config + an API key."
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
        let index = self.index.clone();
        let task = async move { index.reindex().await };
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(ToolResult::err("Cancelled")),
            r = task => r,
        };
        match result {
            Err(e) => Ok(ToolResult::err(format!("Indexing failed: {e}"))),
            Ok((chunks, files)) => Ok(ToolResult::ok(format!(
                "Indexed {chunks} chunks across {files} files into {}",
                self.index.workspace_root().display()
            ))),
        }
    }
}

#[derive(Clone)]
pub struct CodeSearchSemanticTool {
    index: Arc<EmbeddingIndex>,
}

impl CodeSearchSemanticTool {
    pub fn new(index: Arc<EmbeddingIndex>) -> Self {
        Self { index }
    }
}

#[async_trait]
impl Tool for CodeSearchSemanticTool {
    fn name(&self) -> &str {
        "code_search_semantic"
    }
    fn description(&self) -> &str {
        "Semantic code search: 'find me code that does X'. Embeds the query and ranks against indexed chunks by cosine similarity. Run `index_code_embeddings` first. Complements ripgrep (literal) and code_query (structural)."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Natural-language description" },
                "top_k": { "type": "integer", "default": 8 }
            },
            "required": ["query"]
        })
    }
    async fn call(
        &self,
        params: Value,
        _cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: CodeSearchSemanticParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let top_k = p.top_k as usize;
        match self.index.search(&p.query, top_k).await {
            Err(e) => Ok(ToolResult::err(format!("Search failed: {e}"))),
            Ok(hits) if hits.is_empty() => Ok(ToolResult::ok(
                "No matches. Run `index_code_embeddings` if the index is empty or stale."
                    .to_string(),
            )),
            Ok(hits) => {
                let payload = json!({
                    "query": p.query,
                    "matches": hits,
                });
                Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
            }
        }
    }
}
