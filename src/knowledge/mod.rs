//! YYC-194: governance + purge controls for local knowledge
//! indexes (code graph, embeddings, session FTS, run records,
//! artifacts).
//!
//! ## Scope of this PR
//!
//! - `KnowledgeIndex` enum naming each indexed surface.
//! - `KnowledgeStoreInfo` describing a single on-disk store
//!   (path, kind, size, last-modified).
//! - `discover()` walks `~/.vulcan` and reports every present
//!   index with metadata.
//!
//! ## Deliberately deferred
//!
//! - Purge (PR-2).
//! - Exclusion config + retrieval provenance (PR-3).
//! - `doctor` integration (YYC-183).
//! - Workspace trust profile interplay (YYC-182).

use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Logical class of a knowledge index. Each variant maps to a known
/// on-disk artifact under `~/.vulcan` (or per-cwd subdir for
/// workspace-scoped indexes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeIndex {
    /// Code graph DB per workspace (YYC-50).
    CodeGraph,
    /// Embedding chunks per workspace (YYC-48).
    Embeddings,
    /// Session messages + FTS5 (YYC-22 / YYC-42).
    Sessions,
    /// Durable run-record timeline (YYC-179).
    RunRecords,
    /// Typed artifacts (YYC-180).
    Artifacts,
}

impl KnowledgeIndex {
    pub fn as_str(self) -> &'static str {
        match self {
            KnowledgeIndex::CodeGraph => "code_graph",
            KnowledgeIndex::Embeddings => "embeddings",
            KnowledgeIndex::Sessions => "sessions",
            KnowledgeIndex::RunRecords => "run_records",
            KnowledgeIndex::Artifacts => "artifacts",
        }
    }
}

/// One discovered store on disk.
#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeStoreInfo {
    pub kind: KnowledgeIndex,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub modified: Option<chrono::DateTime<chrono::Utc>>,
    /// For workspace-scoped indexes (`code_graph`, `embeddings`),
    /// the sanitized workspace key derived from the filename.
    pub workspace_key: Option<String>,
}

/// Walk `~/.vulcan` and report every present knowledge index.
/// Returns an empty Vec when the home directory doesn't exist
/// — a fresh install isn't an error.
pub fn discover() -> Result<Vec<KnowledgeStoreInfo>> {
    let home = crate::config::vulcan_home();
    discover_in(&home)
}

/// Same as [`discover`] but rooted at an explicit directory —
/// used by tests so they don't touch the real `~/.vulcan`.
pub fn discover_in(home: &Path) -> Result<Vec<KnowledgeStoreInfo>> {
    let mut out = Vec::new();
    if !home.exists() {
        return Ok(out);
    }
    // Top-level singleton stores.
    for (kind, fname) in [
        (KnowledgeIndex::Sessions, "sessions.db"),
        (KnowledgeIndex::RunRecords, "run_records.db"),
        (KnowledgeIndex::Artifacts, "artifacts.db"),
    ] {
        let path = home.join(fname);
        if path.exists() {
            out.push(probe_store(kind, path, None)?);
        }
    }
    // Workspace-scoped subdirs.
    for (kind, dir) in [
        (KnowledgeIndex::CodeGraph, "code_graph"),
        (KnowledgeIndex::Embeddings, "embeddings"),
    ] {
        let dir_path = home.join(dir);
        if !dir_path.is_dir() {
            continue;
        }
        let entries = std::fs::read_dir(&dir_path)?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("db") {
                continue;
            }
            let workspace_key = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string());
            out.push(probe_store(kind, path, workspace_key)?);
        }
    }
    Ok(out)
}

fn probe_store(
    kind: KnowledgeIndex,
    path: PathBuf,
    workspace_key: Option<String>,
) -> Result<KnowledgeStoreInfo> {
    let meta = std::fs::metadata(&path)?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| chrono::DateTime::<chrono::Utc>::from(t).into());
    Ok(KnowledgeStoreInfo {
        kind,
        path,
        size_bytes: meta.len(),
        modified,
        workspace_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn discover_returns_empty_for_missing_home() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("not-here");
        let stores = discover_in(&missing).unwrap();
        assert!(stores.is_empty());
    }

    #[test]
    fn discover_finds_top_level_singletons() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("sessions.db"), b"x").unwrap();
        std::fs::write(dir.path().join("run_records.db"), b"yz").unwrap();
        std::fs::write(dir.path().join("artifacts.db"), b"abc").unwrap();
        let stores = discover_in(dir.path()).unwrap();
        let mut kinds: Vec<&'static str> = stores.iter().map(|s| s.kind.as_str()).collect();
        kinds.sort();
        assert_eq!(kinds, vec!["artifacts", "run_records", "sessions"]);
        let sizes: u64 = stores.iter().map(|s| s.size_bytes).sum();
        assert_eq!(sizes, 1 + 2 + 3);
    }

    #[test]
    fn discover_finds_per_workspace_subdirs() {
        let dir = tempdir().unwrap();
        let cg = dir.path().join("code_graph");
        let emb = dir.path().join("embeddings");
        std::fs::create_dir(&cg).unwrap();
        std::fs::create_dir(&emb).unwrap();
        std::fs::write(cg.join("home_a.db"), b"a").unwrap();
        std::fs::write(cg.join("home_b.db"), b"bb").unwrap();
        std::fs::write(emb.join("home_a.db"), b"ccc").unwrap();
        // Skip non-db files in subdirs.
        std::fs::write(cg.join("readme.txt"), b"skip").unwrap();
        let stores = discover_in(dir.path()).unwrap();
        let cg_count = stores
            .iter()
            .filter(|s| s.kind == KnowledgeIndex::CodeGraph)
            .count();
        let emb_count = stores
            .iter()
            .filter(|s| s.kind == KnowledgeIndex::Embeddings)
            .count();
        assert_eq!(cg_count, 2);
        assert_eq!(emb_count, 1);
        // Workspace key surfaces from the filename stem.
        let mut keys: Vec<String> = stores
            .iter()
            .filter(|s| s.kind == KnowledgeIndex::CodeGraph)
            .filter_map(|s| s.workspace_key.clone())
            .collect();
        keys.sort();
        assert_eq!(keys, vec!["home_a".to_string(), "home_b".to_string()]);
    }
}
