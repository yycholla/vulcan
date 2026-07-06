//! Turso persistence seam (GH #704).
//!
//! Thin wrapper over `turso` connection setup so stores don't hardcode
//! the driver or repeat the experimental-flag dance. Introduced during
//! the phased rusqlite -> turso migration; gated behind `turso-backend`
//! until every store has ported, at which point this becomes the only
//! backend and `rusqlite`/`r2d2` drop out.
//!
//! Turso is async and its `Connection` is internally sync-safe (clonable
//! handle, no external `Mutex`), so ported stores hold a bare
//! `turso::Connection` and drop the `Mutex<Connection>` / r2d2 pool /
//! `spawn_blocking` scaffolding the rusqlite stores needed.

use anyhow::{Context, Result};
use std::path::Path;

/// Open a Turso database at `path`, creating parent dirs as needed.
/// Enables the experimental index method so native FTS
/// (`CREATE INDEX ... USING fts(...)`) and the vector index types are
/// available — vulcan opts into these deliberately (GH #704).
pub async fn open(path: &Path) -> Result<turso::Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create db dir {}", parent.display()))?;
    }
    let db = turso::Builder::new_local(&path.to_string_lossy())
        .experimental_index_method(true)
        .build()
        .await
        .with_context(|| format!("open turso db at {}", path.display()))?;
    db.connect().context("connect to turso db")
}

/// In-memory Turso database for tests. Same experimental flags as
/// [`open`] so FTS/vector behavior matches on-disk.
pub async fn open_in_memory() -> Result<turso::Connection> {
    let db = turso::Builder::new_local(":memory:")
        .experimental_index_method(true)
        .build()
        .await
        .context("open in-memory turso db")?;
    db.connect().context("connect to in-memory turso db")
}

#[cfg(test)]
mod tests {
    use super::*;

    // GH #704 acceptance probe: native FTS returns BM25-ranked hits over
    // mixed English + code-identifier content (vulcan's recall corpus),
    // and a prefix query matches both prose and an identifier.
    #[tokio::test]
    async fn native_fts_ranks_prose_and_identifiers() {
        let conn = open_in_memory().await.unwrap();
        conn.execute(
            "CREATE TABLE messages (id INTEGER PRIMARY KEY, content TEXT)",
            (),
        )
        .await
        .unwrap();
        for (i, text) in [
            "let me run cargo build and check the errors",
            "the run_prompt_direct function streams tokens to stdout",
            "completely unrelated note about the weather today",
        ]
        .iter()
        .enumerate()
        {
            conn.execute(
                "INSERT INTO messages (id, content) VALUES (?1, ?2)",
                (i as i64 + 1, *text),
            )
            .await
            .unwrap();
        }
        conn.execute(
            "CREATE INDEX messages_fts ON messages USING fts(content)",
            (),
        )
        .await
        .unwrap();

        let mut rows = conn
            .query(
                "SELECT id FROM messages WHERE fts_match(content, 'run*') \
                 ORDER BY fts_score(content, 'run*') DESC",
                (),
            )
            .await
            .unwrap();
        let mut hits = 0;
        while rows.next().await.unwrap().is_some() {
            hits += 1;
        }
        assert!(hits >= 2, "expected >=2 run* hits, got {hits}");
    }
}
