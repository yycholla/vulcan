//! Embedding-based code retrieval (YYC-48).
//!
//! Tree-sitter chunking (one chunk per top-level symbol) + a remote
//! OpenAI-compatible embeddings endpoint + brute-force cosine ranking
//! over a SQLite store. Local-model support is deferred — the candle/
//! ort dep cost wasn't worth it for v1, and most users already have an
//! API key for the chat endpoint.
//!
//! Storage: `~/.vulcan/embeddings/<sanitized-cwd>.db`. Per-cwd
//! isolation matches the code-graph (YYC-50) layout.

use crate::code::{Language, ParserCache};
use crate::config::EmbeddingsConfig;
use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tree_sitter::{Query, QueryCursor, StreamingIterator};

#[derive(Debug, Clone)]
pub struct CodeChunk {
    pub file: String,
    pub language: String,
    pub kind: String,
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmbeddingHit {
    pub file: String,
    pub kind: String,
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub score: f32,
}

pub struct EmbeddingIndex {
    conn: Mutex<Connection>,
    workspace_root: PathBuf,
    parsers: Arc<ParserCache>,
    cfg: EmbeddingsConfig,
    /// Provider's chat-endpoint base URL + key, used as the fallback
    /// when [embeddings] doesn't override them (YYC-48).
    fallback_base_url: String,
    fallback_api_key: Option<String>,
    client: reqwest::Client,
}

impl EmbeddingIndex {
    pub fn open(
        workspace_root: PathBuf,
        parsers: Arc<ParserCache>,
        cfg: EmbeddingsConfig,
        fallback_base_url: String,
        fallback_api_key: Option<String>,
    ) -> Result<Self> {
        let db_path = db_path_for(&workspace_root)?;
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(&db_path)
            .with_context(|| format!("open embeddings at {}", db_path.display()))?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS chunks (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                file        TEXT NOT NULL,
                language    TEXT NOT NULL,
                kind        TEXT NOT NULL,
                name        TEXT NOT NULL,
                start_line  INTEGER NOT NULL,
                end_line    INTEGER NOT NULL,
                text        TEXT NOT NULL,
                embedding   BLOB NOT NULL,
                dim         INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_chunks_file ON chunks(file);
            "#,
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            workspace_root,
            parsers,
            cfg,
            fallback_base_url,
            fallback_api_key,
            client: reqwest::Client::new(),
        })
    }

    fn endpoint(&self) -> String {
        let base = if self.cfg.base_url.is_empty() {
            &self.fallback_base_url
        } else {
            &self.cfg.base_url
        };
        format!("{}/embeddings", base.trim_end_matches('/'))
    }

    fn api_key(&self) -> Option<&str> {
        self.cfg
            .api_key
            .as_deref()
            .or(self.fallback_api_key.as_deref())
    }

    /// Embed a batch of strings. Returns one Vec<f32> per input, in
    /// order. Calls the OpenAI-compatible /embeddings endpoint.
    pub async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let key = self
            .api_key()
            .ok_or_else(|| anyhow!("no API key configured for embeddings"))?;
        let url = self.endpoint();
        let body = json!({
            "model": self.cfg.model,
            "input": inputs,
        });
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {key}"))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("embeddings endpoint returned {status}: {text}"));
        }
        let json: serde_json::Value = resp.json().await?;
        let data = json
            .get("data")
            .and_then(|d| d.as_array())
            .ok_or_else(|| anyhow!("no `data` array in embeddings response"))?;
        let mut out = Vec::with_capacity(data.len());
        for entry in data {
            let v = entry
                .get("embedding")
                .and_then(|e| e.as_array())
                .ok_or_else(|| anyhow!("missing embedding in response entry"))?;
            let parsed: Result<Vec<f32>> = v
                .iter()
                .map(|n| {
                    n.as_f64()
                        .map(|f| f as f32)
                        .ok_or_else(|| anyhow!("non-numeric embedding entry"))
                })
                .collect();
            out.push(parsed?);
        }
        Ok(out)
    }

    /// Walk the workspace, chunk source files into top-level symbols,
    /// embed each, persist. Returns `(chunks_indexed, files_visited)`.
    pub async fn reindex(&self) -> Result<(usize, usize)> {
        let walker = ignore::WalkBuilder::new(&self.workspace_root)
            .standard_filters(true)
            .build();
        let mut all_chunks: Vec<CodeChunk> = Vec::new();
        let mut files = 0usize;
        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let path = entry.path();
            let lang = match Language::from_path(path) {
                Some(l) => l,
                None => continue,
            };
            let source = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let rel = path
                .strip_prefix(&self.workspace_root)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            let chunks = chunk_file(&self.parsers, lang, &rel, &source)?;
            files += 1;
            all_chunks.extend(chunks);
        }
        if all_chunks.is_empty() {
            return Ok((0, files));
        }
        // Wipe + repopulate. Incremental updates land as a follow-up.
        {
            let conn = self.conn.lock().unwrap();
            conn.execute("DELETE FROM chunks", [])?;
        }

        // Batch embed in groups of 64 to keep request bodies small and
        // share token budget across files.
        let mut total = 0usize;
        for batch in all_chunks.chunks(64) {
            let inputs: Vec<String> = batch.iter().map(|c| c.text.clone()).collect();
            let vectors = self.embed(&inputs).await?;
            if vectors.len() != batch.len() {
                return Err(anyhow!(
                    "embeddings response had {} entries for {} inputs",
                    vectors.len(),
                    batch.len()
                ));
            }
            let conn = self.conn.lock().unwrap();
            for (chunk, vec) in batch.iter().zip(vectors.into_iter()) {
                let blob = vec_to_bytes(&vec);
                conn.execute(
                    "INSERT INTO chunks (file, language, kind, name, start_line, end_line, text, embedding, dim)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        chunk.file,
                        chunk.language,
                        chunk.kind,
                        chunk.name,
                        chunk.start_line as i64,
                        chunk.end_line as i64,
                        chunk.text,
                        blob,
                        vec.len() as i64,
                    ],
                )?;
                total += 1;
            }
        }
        Ok((total, files))
    }

    /// Embed `query` and return the top-k chunks by cosine similarity.
    /// Brute force — fine up to a few thousand chunks. A vector index
    /// (sqlite-vss / lance) is the obvious next step.
    pub async fn search(&self, query: &str, top_k: usize) -> Result<Vec<EmbeddingHit>> {
        let q = self.embed(&[query.to_string()]).await?;
        let qv = q
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("empty embedding response for query"))?;

        let rows: Vec<(String, String, String, i64, i64, Vec<u8>)> = {
            let conn = self.conn.lock().unwrap();
            let mut stmt = conn.prepare(
                "SELECT file, kind, name, start_line, end_line, embedding FROM chunks",
            )?;
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                ))
            })?
            .collect::<Result<_, _>>()?
        };

        let mut scored: Vec<EmbeddingHit> = rows
            .into_iter()
            .filter_map(|(file, kind, name, start, end, blob)| {
                let v = bytes_to_vec(&blob)?;
                let score = cosine(&qv, &v);
                Some(EmbeddingHit {
                    file,
                    kind,
                    name,
                    start_line: start as usize,
                    end_line: end as usize,
                    score,
                })
            })
            .collect();
        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);
        Ok(scored)
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }
}

fn chunk_file(
    parsers: &ParserCache,
    lang: Language,
    relpath: &str,
    source: &str,
) -> Result<Vec<CodeChunk>> {
    let query_text = lang.outline_query();
    if query_text.is_empty() {
        return Ok(Vec::new());
    }
    parsers.with_parser(lang, |parser| {
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow!("parse failed"))?;
        let grammar = match lang {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            Language::Json => return Ok(Vec::new()),
        };
        let query = Query::new(&grammar, query_text)
            .map_err(|e| anyhow!("query: {e}"))?;
        let name_idx = query.capture_index_for_name("name");
        let mut cursor = QueryCursor::new();
        let mut iter = cursor.matches(&query, tree.root_node(), source.as_bytes());
        let mut out = Vec::new();
        while let Some(m) = iter.next() {
            let mut name = None;
            let mut node = None;
            let mut kind = "symbol".to_string();
            for cap in m.captures {
                let cap_name = &query.capture_names()[cap.index as usize];
                if Some(cap.index) == name_idx {
                    name = Some(
                        cap.node
                            .utf8_text(source.as_bytes())
                            .unwrap_or("")
                            .to_string(),
                    );
                } else {
                    node = Some(cap.node);
                    kind = (*cap_name).to_string();
                }
            }
            if let (Some(n), Some(node)) = (name, node) {
                let start = node.start_position().row + 1;
                let end = node.end_position().row + 1;
                let text = node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
                out.push(CodeChunk {
                    file: relpath.to_string(),
                    language: lang.name().to_string(),
                    kind,
                    name: n,
                    start_line: start,
                    end_line: end,
                    text,
                });
            }
        }
        Ok(out)
    })?
}

fn db_path_for(workspace_root: &Path) -> Result<PathBuf> {
    let home = crate::config::vulcan_home();
    let key = workspace_root
        .to_string_lossy()
        .trim_start_matches('/')
        .replace(['/', '\\'], "_");
    Ok(home.join("embeddings").join(format!("{key}.db")))
}

fn vec_to_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

fn bytes_to_vec(b: &[u8]) -> Option<Vec<f32>> {
    if b.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(b.len() / 4);
    for chunk in b.chunks_exact(4) {
        let arr = [chunk[0], chunk[1], chunk[2], chunk[3]];
        out.push(f32::from_le_bytes(arr));
    }
    Some(out)
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let denom = (na.sqrt() * nb.sqrt()).max(f32::EPSILON);
    dot / denom
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vec_bytes_round_trip() {
        let v = vec![1.0_f32, -2.5, 0.0, 3.14159];
        let b = vec_to_bytes(&v);
        let back = bytes_to_vec(&b).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn cosine_known_values() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!(cosine(&a, &b).abs() < 1e-6);
        let c = vec![1.0, 1.0, 0.0];
        assert!((cosine(&a, &c) - (1.0 / 2.0_f32.sqrt())).abs() < 1e-6);
    }
}
