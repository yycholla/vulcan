//! SQLite-backed code graph (YYC-50).
//!
//! Tracks symbol declarations across the workspace so the agent can
//! ask "where is `foo` defined?" without re-parsing every file. Built
//! from tree-sitter outlines (YYC-45) — fast, no LSP dependency at
//! index time. Call-edges + type-hierarchy edges are deferred to a
//! follow-up; the schema reserves the columns so they can land
//! incrementally without breaking the index.
//!
//! Index location: `~/.vulcan/code_graph/<sanitized-cwd>.db`. Per-cwd
//! isolation so two different projects don't collide.

use crate::code::{Language, ParserCache};
use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use tree_sitter::{Query, QueryCursor, StreamingIterator};

#[derive(Debug, Clone, serde::Serialize)]
pub struct SymbolRow {
    pub file: String,
    pub language: String,
    pub kind: String,
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
}

pub struct CodeGraph {
    conn: Mutex<Connection>,
    workspace_root: PathBuf,
    parsers: Arc<ParserCache>,
}

impl CodeGraph {
    /// Open or create the graph DB for `workspace_root`. The DB lives
    /// under `~/.vulcan/code_graph/<sanitized>.db` so each project
    /// gets its own isolated index.
    pub fn open(workspace_root: PathBuf, parsers: Arc<ParserCache>) -> Result<Self> {
        let db_path = db_path_for(&workspace_root)?;
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(&db_path)
            .with_context(|| format!("open code graph at {}", db_path.display()))?;
        // Schema mirrors the issue's plan — symbols today, edges
        // (calls/implements/inherits) reserved for the call-hierarchy
        // follow-up.
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS symbols (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                file            TEXT NOT NULL,
                language        TEXT NOT NULL,
                kind            TEXT NOT NULL,
                name            TEXT NOT NULL,
                start_line      INTEGER NOT NULL,
                end_line        INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
            CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file);

            CREATE TABLE IF NOT EXISTS calls (
                caller_id INTEGER NOT NULL,
                callee_id INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS implements (
                impl_id  INTEGER NOT NULL,
                trait_id INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS inherits (
                child_id  INTEGER NOT NULL,
                parent_id INTEGER NOT NULL
            );
            "#,
        )
        .context("init code graph schema")?;

        Ok(Self {
            conn: Mutex::new(conn),
            workspace_root,
            parsers,
        })
    }

    /// Walk the workspace, parse every supported source file, and
    /// upsert its symbols. Returns `(files_indexed, symbols_inserted)`.
    /// Respects `.gitignore`. Existing rows for re-indexed files are
    /// dropped first so the operation is idempotent.
    pub fn reindex(&self) -> Result<(usize, usize)> {
        let walker = ignore::WalkBuilder::new(&self.workspace_root)
            .standard_filters(true)
            .build();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let mut files = 0usize;
        let mut symbols = 0usize;
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

            tx.execute("DELETE FROM symbols WHERE file = ?", params![rel])?;
            let extracted = extract_symbols(&self.parsers, lang, &source)?;
            for s in &extracted {
                tx.execute(
                    "INSERT INTO symbols (file, language, kind, name, start_line, end_line)
                     VALUES (?, ?, ?, ?, ?, ?)",
                    params![
                        rel,
                        lang.name(),
                        s.kind,
                        s.name,
                        s.start_line as i64,
                        s.end_line as i64,
                    ],
                )?;
                symbols += 1;
            }
            files += 1;
        }
        tx.commit()?;
        Ok((files, symbols))
    }

    /// Look up symbols by exact name. Used by `find_symbol` tool.
    pub fn find_by_name(&self, name: &str, limit: usize) -> Result<Vec<SymbolRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT file, language, kind, name, start_line, end_line
             FROM symbols WHERE name = ? ORDER BY file LIMIT ?",
        )?;
        let rows = stmt
            .query_map(params![name, limit as i64], |row| {
                Ok(SymbolRow {
                    file: row.get(0)?,
                    language: row.get(1)?,
                    kind: row.get(2)?,
                    name: row.get(3)?,
                    start_line: row.get::<_, i64>(4)? as usize,
                    end_line: row.get::<_, i64>(5)? as usize,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Total indexed symbol count — used by the index status report.
    pub fn count(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }
}

fn db_path_for(workspace_root: &Path) -> Result<PathBuf> {
    let home = crate::config::vulcan_home();
    // Sanitize cwd into a filename. Replace path separators with `_`
    // so e.g. "/home/foo/bar" → "home_foo_bar.db".
    let key = workspace_root
        .to_string_lossy()
        .trim_start_matches('/')
        .replace(['/', '\\'], "_");
    Ok(home.join("code_graph").join(format!("{key}.db")))
}

fn extract_symbols(
    parsers: &ParserCache,
    lang: Language,
    source: &str,
) -> Result<Vec<SymbolRow>> {
    let query_text = lang.outline_query();
    if query_text.is_empty() {
        return Ok(Vec::new());
    }
    parsers.with_parser(lang, |parser| {
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("parse failed"))?;
        let grammar = match lang {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            Language::Json => return Ok(Vec::new()),
        };
        let query = Query::new(&grammar, query_text)
            .map_err(|e| anyhow::anyhow!("query: {e}"))?;
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
                out.push(SymbolRow {
                    file: String::new(),
                    language: lang.name().to_string(),
                    kind,
                    name: n,
                    start_line: node.start_position().row + 1,
                    end_line: node.end_position().row + 1,
                });
            }
        }
        Ok(out)
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn reindex_and_find_symbol_round_trip() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn alpha() {}\nstruct Beta;\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn alpha() {}\n").unwrap();
        let graph =
            CodeGraph::open(dir.path().to_path_buf(), Arc::new(ParserCache::new())).unwrap();
        let (files, symbols) = graph.reindex().unwrap();
        assert_eq!(files, 2);
        assert!(symbols >= 3, "expected ≥3 symbols, got {symbols}");

        let alphas = graph.find_by_name("alpha", 10).unwrap();
        assert_eq!(alphas.len(), 2);
        let betas = graph.find_by_name("Beta", 10).unwrap();
        assert_eq!(betas.len(), 1);
        assert_eq!(betas[0].kind, "struct");
    }

    #[test]
    fn reindex_replaces_stale_rows() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn old_name() {}\n").unwrap();
        let graph =
            CodeGraph::open(dir.path().to_path_buf(), Arc::new(ParserCache::new())).unwrap();
        graph.reindex().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn new_name() {}\n").unwrap();
        graph.reindex().unwrap();
        assert!(graph.find_by_name("old_name", 5).unwrap().is_empty());
        assert_eq!(graph.find_by_name("new_name", 5).unwrap().len(), 1);
    }
}
