//! SQLite-backed code graph (YYC-50).
//!
//! Tracks symbol declarations and LSP-backed relationships across the
//! workspace so the agent can ask "where is `foo` defined?" without
//! re-parsing every file, and later graph queries can reason about
//! calls, type definitions, and implementation hierarchies. Symbol
//! discovery stays Tree-sitter-first; edge harvesting is best-effort
//! and explicitly tolerates missing/incomplete LSP servers.
//!
//! Index location: `~/.vulcan/code_graph/<sanitized-cwd>.db`. Per-cwd
//! isolation so two different projects don't collide.

use crate::code::{Language, ParserCache};
use anyhow::{Context, Result};
use lsp_types::{CallHierarchyItem, Location};
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tree_sitter::{Query, QueryCursor, StreamingIterator};

#[derive(Debug, Clone, serde::Serialize)]
pub struct SymbolRow {
    pub file: String,
    pub language: String,
    pub kind: String,
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub start_character: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum EdgeKind {
    Call,
    TypeDefinition,
    Implementation,
    Inheritance,
}

impl EdgeKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::TypeDefinition => "type_definition",
            Self::Implementation => "implementation",
            Self::Inheritance => "inheritance",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "call" => Self::Call,
            "type_definition" => Self::TypeDefinition,
            "implementation" => Self::Implementation,
            "inheritance" => Self::Inheritance,
            _ => Self::Call,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum EdgeDirection {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EdgeQueryResult {
    pub source_symbol: String,
    pub direction: EdgeDirection,
    pub edge_kind: EdgeKind,
    pub edges: Vec<CodeGraphEdge>,
    pub limit: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TypeHierarchyResult {
    pub source_symbol: String,
    pub implementations: Vec<CodeGraphEdge>,
    pub subtypes: Vec<CodeGraphEdge>,
    pub supertypes: Vec<CodeGraphEdge>,
    pub traversed_edge_kinds: Vec<EdgeKind>,
    pub limit: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ImpactedSymbol {
    pub symbol: String,
    pub file: String,
    pub start_line: usize,
    pub start_character: usize,
    pub depth: usize,
    pub via_edge: CodeGraphEdge,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ImpactAnalysisResult {
    pub source_symbol: String,
    pub traversed_edge_kinds: Vec<EdgeKind>,
    pub max_depth: usize,
    pub limit: usize,
    pub impacted_symbols: Vec<ImpactedSymbol>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum EdgeProvider {
    Lsp,
}

impl EdgeProvider {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lsp => "lsp",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "lsp" => Self::Lsp,
            _ => Self::Lsp,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CodeGraphEdge {
    pub kind: EdgeKind,
    pub source_file: String,
    pub source_name: Option<String>,
    pub source_start_line: usize,
    pub source_start_character: usize,
    pub target_file: String,
    pub target_name: Option<String>,
    pub target_start_line: usize,
    pub target_start_character: usize,
    pub provider: EdgeProvider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum LspIndexStatus {
    Unavailable,
    Complete,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CodeGraphIndexReport {
    pub files_indexed: usize,
    pub symbols_inserted: usize,
    pub edges_inserted: usize,
    pub lsp_status: LspIndexStatus,
    pub lsp_errors: Vec<String>,
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
                end_line        INTEGER NOT NULL,
                start_character INTEGER NOT NULL DEFAULT 1
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

            CREATE TABLE IF NOT EXISTS graph_edges (
                id                     INTEGER PRIMARY KEY AUTOINCREMENT,
                kind                   TEXT NOT NULL,
                source_file            TEXT NOT NULL,
                source_name            TEXT,
                source_start_line      INTEGER NOT NULL,
                source_start_character INTEGER NOT NULL,
                target_file            TEXT NOT NULL,
                target_name            TEXT,
                target_start_line      INTEGER NOT NULL,
                target_start_character INTEGER NOT NULL,
                provider               TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_graph_edges_source ON graph_edges(source_file, source_name);
            CREATE INDEX IF NOT EXISTS idx_graph_edges_target ON graph_edges(target_file, target_name);
            CREATE INDEX IF NOT EXISTS idx_graph_edges_kind ON graph_edges(kind);
            "#,
        )
        .context("init code graph schema")?;
        ensure_column(
            &conn,
            "symbols",
            "start_character",
            "INTEGER NOT NULL DEFAULT 1",
        )?;

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
        let mut conn = self.conn.lock();
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
            tx.execute(
                "DELETE FROM graph_edges WHERE source_file = ? OR target_file = ?",
                params![rel, rel],
            )?;
            let extracted = extract_symbols(&self.parsers, lang, &source)?;
            for s in &extracted {
                tx.execute(
                    "INSERT INTO symbols (file, language, kind, name, start_line, end_line, start_character)
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
                    params![
                        rel,
                        lang.name(),
                        s.kind,
                        s.name,
                        s.start_line as i64,
                        s.end_line as i64,
                        s.start_character as i64,
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
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT file, language, kind, name, start_line, end_line, start_character
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
                    start_character: row.get::<_, i64>(6)? as usize,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Reindex symbols and optionally persist a known edge set. `None`
    /// is the explicit missing-LSP path: symbol indexing still succeeds
    /// and stale edges are cleared by `reindex`.
    pub fn reindex_with_edges(
        &self,
        edges: Option<&[CodeGraphEdge]>,
    ) -> Result<CodeGraphIndexReport> {
        let (files_indexed, symbols_inserted) = self.reindex()?;
        let edges_inserted = match edges {
            Some(edges) => {
                self.replace_all_edges(edges)?;
                edges.len()
            }
            None => 0,
        };
        Ok(CodeGraphIndexReport {
            files_indexed,
            symbols_inserted,
            edges_inserted,
            lsp_status: if edges.is_some() {
                LspIndexStatus::Complete
            } else {
                LspIndexStatus::Unavailable
            },
            lsp_errors: Vec::new(),
        })
    }

    pub fn replace_edges_for_file(&self, file: &str, edges: &[CodeGraphEdge]) -> Result<usize> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM graph_edges WHERE source_file = ?",
            params![file],
        )?;
        insert_edges(&tx, edges)?;
        tx.commit()?;
        Ok(edges.len())
    }

    pub fn replace_all_edges(&self, edges: &[CodeGraphEdge]) -> Result<usize> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM graph_edges", [])?;
        insert_edges(&tx, edges)?;
        tx.commit()?;
        Ok(edges.len())
    }

    pub fn edges_for_file(&self, file: &str) -> Result<Vec<CodeGraphEdge>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT kind, source_file, source_name, source_start_line, source_start_character,
                    target_file, target_name, target_start_line, target_start_character, provider
             FROM graph_edges
             WHERE source_file = ? OR target_file = ?
             ORDER BY kind, source_file, target_file, target_name",
        )?;
        let rows = stmt
            .query_map(params![file, file], edge_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn find_callers(&self, symbol: &str, limit: usize) -> Result<EdgeQueryResult> {
        self.query_edges_by_symbol(symbol, EdgeKind::Call, EdgeDirection::Incoming, limit)
    }

    pub fn find_callees(&self, symbol: &str, limit: usize) -> Result<EdgeQueryResult> {
        self.query_edges_by_symbol(symbol, EdgeKind::Call, EdgeDirection::Outgoing, limit)
    }

    pub fn type_hierarchy(&self, symbol: &str, limit: usize) -> Result<TypeHierarchyResult> {
        let implementations = self.edge_query(
            EdgeKind::Implementation,
            EdgeDirection::Incoming,
            symbol,
            limit.saturating_add(1),
        )?;
        let subtypes = self.edge_query(
            EdgeKind::Inheritance,
            EdgeDirection::Incoming,
            symbol,
            limit.saturating_add(1),
        )?;
        let supertypes = self.edge_query(
            EdgeKind::Inheritance,
            EdgeDirection::Outgoing,
            symbol,
            limit.saturating_add(1),
        )?;
        let mut remaining = limit;
        let (implementations, impl_truncated) = take_limited(implementations, &mut remaining);
        let (subtypes, subtype_truncated) = take_limited(subtypes, &mut remaining);
        let (supertypes, supertype_truncated) = take_limited(supertypes, &mut remaining);
        Ok(TypeHierarchyResult {
            source_symbol: symbol.to_string(),
            implementations,
            subtypes,
            supertypes,
            traversed_edge_kinds: vec![EdgeKind::Implementation, EdgeKind::Inheritance],
            limit,
            truncated: impl_truncated || subtype_truncated || supertype_truncated,
        })
    }

    pub fn impact_analysis(
        &self,
        symbol: &str,
        max_depth: usize,
        limit: usize,
    ) -> Result<ImpactAnalysisResult> {
        let mut impacted_symbols = Vec::new();
        let mut visited = HashSet::new();
        visited.insert(symbol.to_string());
        let mut queue = VecDeque::from([(symbol.to_string(), 0usize)]);
        let mut truncated = false;

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let callers = self.edge_query(
                EdgeKind::Call,
                EdgeDirection::Incoming,
                &current,
                limit.saturating_add(1),
            )?;
            for edge in callers {
                let Some(source) = edge.source_name.clone() else {
                    continue;
                };
                if !visited.insert(source.clone()) {
                    continue;
                }
                if impacted_symbols.len() >= limit {
                    truncated = true;
                    break;
                }
                impacted_symbols.push(ImpactedSymbol {
                    symbol: source.clone(),
                    file: edge.source_file.clone(),
                    start_line: edge.source_start_line,
                    start_character: edge.source_start_character,
                    depth: depth + 1,
                    via_edge: edge,
                });
                queue.push_back((source, depth + 1));
            }
            if truncated {
                break;
            }
        }

        Ok(ImpactAnalysisResult {
            source_symbol: symbol.to_string(),
            traversed_edge_kinds: vec![EdgeKind::Call],
            max_depth,
            limit,
            impacted_symbols,
            truncated: truncated || !queue.is_empty(),
        })
    }

    fn query_edges_by_symbol(
        &self,
        symbol: &str,
        kind: EdgeKind,
        direction: EdgeDirection,
        limit: usize,
    ) -> Result<EdgeQueryResult> {
        let mut edges = self.edge_query(kind, direction, symbol, limit.saturating_add(1))?;
        let truncated = edges.len() > limit;
        edges.truncate(limit);
        Ok(EdgeQueryResult {
            source_symbol: symbol.to_string(),
            direction,
            edge_kind: kind,
            edges,
            limit,
            truncated,
        })
    }

    fn edge_query(
        &self,
        kind: EdgeKind,
        direction: EdgeDirection,
        symbol: &str,
        limit: usize,
    ) -> Result<Vec<CodeGraphEdge>> {
        let conn = self.conn.lock();
        let predicate = match direction {
            EdgeDirection::Incoming => "target_name = ?",
            EdgeDirection::Outgoing => "source_name = ?",
        };
        let mut stmt = conn.prepare(&format!(
            "SELECT kind, source_file, source_name, source_start_line, source_start_character,
                    target_file, target_name, target_start_line, target_start_character, provider
             FROM graph_edges
             WHERE kind = ? AND {predicate}
             ORDER BY source_name, target_name, source_file, target_file
             LIMIT ?"
        ))?;
        let rows = stmt
            .query_map(params![kind.as_str(), symbol, limit as i64], edge_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn indexed_symbols(&self) -> Result<Vec<SymbolRow>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT file, language, kind, name, start_line, end_line, start_character
             FROM symbols ORDER BY file, start_line",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(SymbolRow {
                    file: row.get(0)?,
                    language: row.get(1)?,
                    kind: row.get(2)?,
                    name: row.get(3)?,
                    start_line: row.get::<_, i64>(4)? as usize,
                    end_line: row.get::<_, i64>(5)? as usize,
                    start_character: row.get::<_, i64>(6)? as usize,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub async fn harvest_lsp_edges(
        &self,
        manager: &crate::code::lsp::LspManager,
    ) -> Result<CodeGraphIndexReport> {
        let symbols = self.indexed_symbols()?;
        let mut edges = Vec::new();
        let mut errors = Vec::new();

        for symbol in symbols {
            let lang = match Language::from_name(&symbol.language) {
                Some(lang) if supports_lsp_edges(symbol.kind.as_str()) => lang,
                _ => continue,
            };
            let server = match manager.server(lang).await {
                Ok(server) => server,
                Err(err) => {
                    errors.push(format!("{}: {err:#}", symbol.language));
                    continue;
                }
            };
            let abs_path = self.workspace_root.join(&symbol.file);
            let line0 = symbol.start_line.saturating_sub(1) as u32;
            let col0 = symbol.start_character.saturating_sub(1) as u32;

            match crate::code::lsp::prepare_call_hierarchy(&server, &abs_path, line0, col0).await {
                Ok(items) => {
                    for item in items {
                        match crate::code::lsp::call_hierarchy_outgoing(&server, item.clone()).await
                        {
                            Ok(calls) => {
                                for call in calls {
                                    edges.push(edge_from_call_item(
                                        EdgeKind::Call,
                                        &symbol,
                                        &call.to,
                                        &self.workspace_root,
                                    ));
                                }
                            }
                            Err(err) => errors
                                .push(format!("callHierarchy/outgoing {}: {err:#}", symbol.name)),
                        }
                        match crate::code::lsp::call_hierarchy_incoming(&server, item.clone()).await
                        {
                            Ok(calls) => {
                                for call in calls {
                                    edges.push(edge_to_call_item(
                                        EdgeKind::Call,
                                        &call.from,
                                        &symbol,
                                        &self.workspace_root,
                                    ));
                                }
                            }
                            Err(err) => errors
                                .push(format!("callHierarchy/incoming {}: {err:#}", symbol.name)),
                        }
                    }
                }
                Err(err) => errors.push(format!("prepareCallHierarchy {}: {err:#}", symbol.name)),
            }

            match crate::code::lsp::type_definition(&server, &abs_path, line0, col0).await {
                Ok(Some(locations)) => {
                    edges.extend(locations.into_iter().map(|loc| {
                        edge_from_location(
                            EdgeKind::TypeDefinition,
                            &symbol,
                            loc,
                            &self.workspace_root,
                        )
                    }));
                }
                Ok(None) => {}
                Err(err) => errors.push(format!("typeDefinition {}: {err:#}", symbol.name)),
            }

            if matches!(symbol.kind.as_str(), "trait" | "interface") {
                match crate::code::lsp::implementation(&server, &abs_path, line0, col0).await {
                    Ok(Some(locations)) => {
                        edges.extend(locations.into_iter().map(|loc| {
                            edge_to_location(
                                EdgeKind::Implementation,
                                loc,
                                &symbol,
                                &self.workspace_root,
                            )
                        }));
                    }
                    Ok(None) => {}
                    Err(err) => errors.push(format!("implementation {}: {err:#}", symbol.name)),
                }
            }
        }

        self.replace_all_edges(&edges)?;
        let status = if errors.is_empty() {
            LspIndexStatus::Complete
        } else if edges.is_empty() {
            LspIndexStatus::Unavailable
        } else {
            LspIndexStatus::Partial
        };
        Ok(CodeGraphIndexReport {
            files_indexed: 0,
            symbols_inserted: self.count()?,
            edges_inserted: edges.len(),
            lsp_status: status,
            lsp_errors: errors,
        })
    }

    /// Total indexed symbol count — used by the index status report.
    pub fn count(&self) -> Result<usize> {
        let conn = self.conn.lock();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }
}

fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .any(|name| name == column);
    if !exists {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )?;
    }
    Ok(())
}

fn insert_edges(conn: &Connection, edges: &[CodeGraphEdge]) -> Result<()> {
    for e in edges {
        conn.execute(
            "INSERT INTO graph_edges (
                kind, source_file, source_name, source_start_line, source_start_character,
                target_file, target_name, target_start_line, target_start_character, provider
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                e.kind.as_str(),
                e.source_file,
                e.source_name,
                e.source_start_line as i64,
                e.source_start_character as i64,
                e.target_file,
                e.target_name,
                e.target_start_line as i64,
                e.target_start_character as i64,
                e.provider.as_str(),
            ],
        )?;
    }
    Ok(())
}

fn edge_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CodeGraphEdge> {
    let kind: String = row.get(0)?;
    let provider: String = row.get(9)?;
    Ok(CodeGraphEdge {
        kind: EdgeKind::from_str(&kind),
        source_file: row.get(1)?,
        source_name: row.get(2)?,
        source_start_line: row.get::<_, i64>(3)? as usize,
        source_start_character: row.get::<_, i64>(4)? as usize,
        target_file: row.get(5)?,
        target_name: row.get(6)?,
        target_start_line: row.get::<_, i64>(7)? as usize,
        target_start_character: row.get::<_, i64>(8)? as usize,
        provider: EdgeProvider::from_str(&provider),
    })
}

fn take_limited(
    mut edges: Vec<CodeGraphEdge>,
    remaining: &mut usize,
) -> (Vec<CodeGraphEdge>, bool) {
    let truncated = edges.len() > *remaining;
    edges.truncate(*remaining);
    *remaining = remaining.saturating_sub(edges.len());
    (edges, truncated)
}

fn supports_lsp_edges(kind: &str) -> bool {
    matches!(
        kind,
        "function" | "method" | "trait" | "interface" | "class" | "struct" | "impl" | "type"
    )
}

fn edge_from_call_item(
    kind: EdgeKind,
    source: &SymbolRow,
    target: &CallHierarchyItem,
    root: &Path,
) -> CodeGraphEdge {
    CodeGraphEdge {
        kind,
        source_file: source.file.clone(),
        source_name: Some(source.name.clone()),
        source_start_line: source.start_line,
        source_start_character: source.start_character,
        target_file: rel_from_uri(&target.uri, root),
        target_name: Some(target.name.clone()),
        target_start_line: target.range.start.line as usize + 1,
        target_start_character: target.range.start.character as usize + 1,
        provider: EdgeProvider::Lsp,
    }
}

fn edge_to_call_item(
    kind: EdgeKind,
    source: &CallHierarchyItem,
    target: &SymbolRow,
    root: &Path,
) -> CodeGraphEdge {
    CodeGraphEdge {
        kind,
        source_file: rel_from_uri(&source.uri, root),
        source_name: Some(source.name.clone()),
        source_start_line: source.range.start.line as usize + 1,
        source_start_character: source.range.start.character as usize + 1,
        target_file: target.file.clone(),
        target_name: Some(target.name.clone()),
        target_start_line: target.start_line,
        target_start_character: target.start_character,
        provider: EdgeProvider::Lsp,
    }
}

fn edge_from_location(
    kind: EdgeKind,
    source: &SymbolRow,
    target: Location,
    root: &Path,
) -> CodeGraphEdge {
    CodeGraphEdge {
        kind,
        source_file: source.file.clone(),
        source_name: Some(source.name.clone()),
        source_start_line: source.start_line,
        source_start_character: source.start_character,
        target_file: rel_from_uri(&target.uri, root),
        target_name: None,
        target_start_line: target.range.start.line as usize + 1,
        target_start_character: target.range.start.character as usize + 1,
        provider: EdgeProvider::Lsp,
    }
}

fn edge_to_location(
    kind: EdgeKind,
    source: Location,
    target: &SymbolRow,
    root: &Path,
) -> CodeGraphEdge {
    CodeGraphEdge {
        kind,
        source_file: rel_from_uri(&source.uri, root),
        source_name: None,
        source_start_line: source.range.start.line as usize + 1,
        source_start_character: source.range.start.character as usize + 1,
        target_file: target.file.clone(),
        target_name: Some(target.name.clone()),
        target_start_line: target.start_line,
        target_start_character: target.start_character,
        provider: EdgeProvider::Lsp,
    }
}

fn rel_from_uri(uri: &lsp_types::Uri, root: &Path) -> String {
    let s = uri.to_string();
    let path = s.strip_prefix("file://").unwrap_or(&s);
    let pb = PathBuf::from(path);
    pb.strip_prefix(root)
        .unwrap_or(&pb)
        .to_string_lossy()
        .into_owned()
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

fn extract_symbols(parsers: &ParserCache, lang: Language, source: &str) -> Result<Vec<SymbolRow>> {
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
        let query = Query::new(&grammar, query_text).map_err(|e| anyhow::anyhow!("query: {e}"))?;
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
                    name = Some((
                        cap.node
                            .utf8_text(source.as_bytes())
                            .unwrap_or("")
                            .to_string(),
                        cap.node,
                    ));
                } else {
                    node = Some(cap.node);
                    kind = (*cap_name).to_string();
                }
            }
            if let (Some((n, name_node)), Some(node)) = (name, node) {
                out.push(SymbolRow {
                    file: String::new(),
                    language: lang.name().to_string(),
                    kind,
                    name: n,
                    start_line: node.start_position().row + 1,
                    end_line: node.end_position().row + 1,
                    start_character: name_node.start_position().column + 1,
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

    #[test]
    fn persists_lsp_backed_call_and_type_edges() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "fn caller() { callee(); }\nfn callee() {}\ntrait Service {}\nstruct Impl;\n",
        )
        .unwrap();
        let graph =
            CodeGraph::open(dir.path().to_path_buf(), Arc::new(ParserCache::new())).unwrap();
        graph.reindex().unwrap();

        graph
            .replace_edges_for_file(
                "a.rs",
                &[
                    CodeGraphEdge {
                        kind: EdgeKind::Call,
                        source_file: "a.rs".into(),
                        source_name: Some("caller".into()),
                        source_start_line: 1,
                        source_start_character: 1,
                        target_file: "a.rs".into(),
                        target_name: Some("callee".into()),
                        target_start_line: 2,
                        target_start_character: 1,
                        provider: EdgeProvider::Lsp,
                    },
                    CodeGraphEdge {
                        kind: EdgeKind::Implementation,
                        source_file: "a.rs".into(),
                        source_name: Some("Impl".into()),
                        source_start_line: 4,
                        source_start_character: 1,
                        target_file: "a.rs".into(),
                        target_name: Some("Service".into()),
                        target_start_line: 3,
                        target_start_character: 1,
                        provider: EdgeProvider::Lsp,
                    },
                    CodeGraphEdge {
                        kind: EdgeKind::TypeDefinition,
                        source_file: "a.rs".into(),
                        source_name: Some("caller".into()),
                        source_start_line: 1,
                        source_start_character: 1,
                        target_file: "a.rs".into(),
                        target_name: Some("Impl".into()),
                        target_start_line: 4,
                        target_start_character: 1,
                        provider: EdgeProvider::Lsp,
                    },
                ],
            )
            .unwrap();

        let edges = graph.edges_for_file("a.rs").unwrap();
        assert_eq!(edges.len(), 3);
        assert!(edges.iter().any(|e| e.kind == EdgeKind::Call
            && e.source_name.as_deref() == Some("caller")
            && e.target_name.as_deref() == Some("callee")));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::Implementation));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::TypeDefinition));
    }

    #[test]
    fn reindex_without_lsp_keeps_partial_symbol_index_and_clears_edges() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn alpha() {}\n").unwrap();
        let graph =
            CodeGraph::open(dir.path().to_path_buf(), Arc::new(ParserCache::new())).unwrap();
        graph.reindex().unwrap();
        graph
            .replace_edges_for_file(
                "a.rs",
                &[CodeGraphEdge {
                    kind: EdgeKind::Call,
                    source_file: "a.rs".into(),
                    source_name: Some("alpha".into()),
                    source_start_line: 1,
                    source_start_character: 1,
                    target_file: "a.rs".into(),
                    target_name: Some("alpha".into()),
                    target_start_line: 1,
                    target_start_character: 1,
                    provider: EdgeProvider::Lsp,
                }],
            )
            .unwrap();

        let report = graph.reindex_with_edges(None).unwrap();

        assert_eq!(report.files_indexed, 1);
        assert!(report.symbols_inserted >= 1);
        assert_eq!(report.edges_inserted, 0);
        assert_eq!(report.lsp_status, LspIndexStatus::Unavailable);
        assert!(graph.edges_for_file("a.rs").unwrap().is_empty());
        assert_eq!(graph.find_by_name("alpha", 5).unwrap().len(), 1);
    }

    #[test]
    fn callers_and_callees_return_bounded_structured_edges() {
        let graph = graph_with_edges(&[
            edge(EdgeKind::Call, "caller", "middle"),
            edge(EdgeKind::Call, "other_caller", "middle"),
            edge(EdgeKind::Call, "middle", "leaf"),
        ]);

        let callers = graph.find_callers("middle", 1).unwrap();
        assert_eq!(callers.source_symbol, "middle");
        assert_eq!(callers.direction, EdgeDirection::Incoming);
        assert_eq!(callers.edges.len(), 1);
        assert_eq!(callers.edges[0].source_name.as_deref(), Some("caller"));
        assert!(callers.truncated);
        assert_eq!(callers.limit, 1);

        let callees = graph.find_callees("middle", 10).unwrap();
        assert_eq!(callees.source_symbol, "middle");
        assert_eq!(callees.direction, EdgeDirection::Outgoing);
        assert_eq!(callees.edges.len(), 1);
        assert_eq!(callees.edges[0].target_name.as_deref(), Some("leaf"));
        assert!(!callees.truncated);
    }

    #[test]
    fn type_hierarchy_returns_implementors_and_declared_parents() {
        let graph = graph_with_edges(&[
            edge(EdgeKind::Implementation, "ImplOne", "Service"),
            edge(EdgeKind::Implementation, "ImplTwo", "Service"),
            edge(EdgeKind::Inheritance, "ChildService", "Service"),
            edge(EdgeKind::TypeDefinition, "factory", "Service"),
        ]);

        let hierarchy = graph.type_hierarchy("Service", 10).unwrap();
        assert_eq!(hierarchy.source_symbol, "Service");
        assert_eq!(hierarchy.implementations.len(), 2);
        assert_eq!(hierarchy.subtypes.len(), 1);
        assert_eq!(hierarchy.supertypes.len(), 0);
        assert!(!hierarchy.truncated);
        assert!(
            hierarchy
                .traversed_edge_kinds
                .contains(&EdgeKind::Implementation)
        );
        assert!(
            hierarchy
                .traversed_edge_kinds
                .contains(&EdgeKind::Inheritance)
        );
    }

    #[test]
    fn impact_analysis_traverses_reverse_call_graph_with_depth_and_limit_bounds() {
        let graph = graph_with_edges(&[
            edge(EdgeKind::Call, "entry", "middle"),
            edge(EdgeKind::Call, "middle", "leaf"),
            edge(EdgeKind::Call, "other", "leaf"),
            edge(EdgeKind::Call, "ignored", "entry"),
        ]);

        let impact = graph.impact_analysis("leaf", 2, 2).unwrap();
        assert_eq!(impact.source_symbol, "leaf");
        assert_eq!(impact.max_depth, 2);
        assert_eq!(impact.limit, 2);
        assert_eq!(impact.impacted_symbols.len(), 2);
        assert_eq!(impact.impacted_symbols[0].symbol, "middle");
        assert_eq!(impact.impacted_symbols[0].depth, 1);
        assert_eq!(impact.impacted_symbols[1].symbol, "other");
        assert_eq!(impact.impacted_symbols[1].depth, 1);
        assert!(impact.truncated);
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
