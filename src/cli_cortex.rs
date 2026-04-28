//! YYC-264: CLI commands for the embedded Cortex graph memory.
//!
//! Provides the `vulcan cortex` subcommand tree: store facts, semantic search,
//! graph statistics, seed from SQLite sessions, and recall recent entries.
//! All operations open the same cortex.redb database that the agent hooks use,
//! so CLI stores are visible to the recall hook and vice versa.

use anyhow::Result;
use chrono::{DateTime, Utc};
use std::path::PathBuf;
use std::sync::Arc;

use crate::cli::CortexSubcommand;
use crate::config::{CortexConfig, vulcan_home};
use crate::memory::SessionStore;
use crate::memory::cortex::CortexStore;

pub async fn run(cmd: CortexSubcommand) -> Result<()> {
    match cmd {
        CortexSubcommand::Store { text, importance } => {
            cmd_store(&text, importance).await?;
        }
        CortexSubcommand::Search { query, limit } => {
            cmd_search(&query, limit).await?;
        }
        CortexSubcommand::Stats => {
            cmd_stats().await?;
        }
        CortexSubcommand::Seed { sessions } => {
            cmd_seed(sessions).await?;
        }
        CortexSubcommand::Recall { limit } => {
            cmd_recall(limit).await?;
        }
    }
    Ok(())
}

/// Resolve the cortex config, open the store, and return it.
fn open_cortex() -> Result<Arc<CortexStore>> {
    let config = CortexConfig::default_enabled();
    CortexStore::try_open(&config)
}

// ── subcommand implementations ──

/// `vulcan cortex store <text>`
async fn cmd_store(text: &str, importance: f32) -> Result<()> {
    let store = open_cortex()?;
    let title = text.chars().take(80).collect::<String>();
    let body = text.to_string();
    let node = CortexStore::fact_with_body(&title, &body, importance);
    let id = store.store(node)?;
    println!("Stored fact: {id}");
    Ok(())
}

/// `vulcan cortex search <query>`
async fn cmd_search(query: &str, limit: usize) -> Result<()> {
    let store = open_cortex()?;
    let results = store.search(query, limit)?;
    if results.is_empty() {
        println!("No results matching: {query}");
        return Ok(());
    }
    println!("Top {} results for \"{query}\":\n", results.len());
    for (score, node) in &results {
        let pct = (score * 100.0) as u8;
        let title = &node.data.title;
        let kind = node.kind.as_str();
        let created = format_datetime(node.created_at);
        println!("  [{kind}] {title} ({pct}%)");
        println!("         Created: {created}");
        let body = &node.data.body;
        if !body.is_empty() {
            let preview: String = body.chars().take(200).collect();
            println!("         {}", preview.replace('\n', " "));
        }
        println!();
    }
    Ok(())
}

/// `vulcan cortex stats`
async fn cmd_stats() -> Result<()> {
    let store = open_cortex()?;
    let stats = store.stats()?;

    // DB file size
    let cfg = store.config();
    let db_path = resolve_db_path(cfg);
    let size = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);

    println!("Cortex Graph Memory\n");
    println!("  Nodes:     {}", stats.nodes);
    println!("  Edges:     {}", stats.edges);
    println!("  DB size:   {}", format_bytes(size));
    println!("  DB path:   {}", db_path.display());
    println!("  Embedding: {}", cfg.embedding_model);
    println!("  Enabled:   {}", cfg.enabled);
    Ok(())
}

/// `vulcan cortex seed --sessions <N>`
async fn cmd_seed(sessions: usize) -> Result<()> {
    let count = seed_from_sessions(sessions).await?;
    if count == 0 {
        println!("No sessions to seed from.");
    }
    Ok(())
}

/// Seed the cortex knowledge graph from recent SQLite sessions.
/// Public so main.rs can call it when `--seed-cortex` is set.
/// Returns the number of facts stored.
pub async fn seed_from_sessions(sessions: usize) -> Result<usize> {
    let store = open_cortex()?;
    let session_store = SessionStore::try_new()?;

    let limit = sessions.max(1).min(20);
    let list = session_store.list_sessions(limit)?;

    if list.is_empty() {
        return Ok(0);
    }

    let mut stored = 0usize;
    for summary in &list {
        let history = match session_store.load_history(&summary.id) {
            Ok(Some(h)) => h,
            _ => continue,
        };
        for msg in &history {
            let text = match msg {
                crate::provider::Message::User { content } if !content.trim().is_empty() => {
                    content.clone()
                }
                crate::provider::Message::Assistant { content, .. } => {
                    if let Some(c) = content {
                        if !c.trim().is_empty() {
                            c.clone()
                        } else {
                            continue;
                        }
                    } else {
                        continue;
                    }
                }
                _ => continue,
            };

            let title = text.chars().take(80).collect::<String>();
            let node = CortexStore::fact(&title, 0.3);
            if store.store(node).is_ok() {
                stored += 1;
            }
        }
    }

    println!("Seeded {stored} facts from {} session(s).", list.len());
    Ok(stored)
}

/// `vulcan cortex recall --limit <N>`
async fn cmd_recall(limit: usize) -> Result<()> {
    let store = open_cortex()?;
    // Use an empty query to get recent nodes — cortex-memory-core returns
    // results ordered by recency when the query is short enough.
    // We use a broad search to list recent items.
    let all = store
        .inner
        .list_nodes(cortex_core::NodeFilter::new().with_limit(limit.max(50)))?;

    if all.is_empty() {
        println!("No facts in the graph yet.");
        return Ok(());
    }

    println!("Recent memory (cortex graph):\n");
    for node in all.iter().take(limit) {
        let created = format_datetime(node.created_at);
        let title = &node.data.title;
        let kind = node.kind.as_str();
        println!("  [{kind}] {title}");
        println!("         {created}");
        let body = &node.data.body;
        if !body.is_empty() {
            let preview: String = body.chars().take(160).collect();
            println!("         {}", preview.replace('\n', " "));
        }
        println!();
    }
    println!("Showing {} of {} node(s).", limit.min(all.len()), all.len());
    Ok(())
}

// ── helpers ──

fn resolve_db_path(config: &CortexConfig) -> PathBuf {
    config
        .db_path
        .clone()
        .unwrap_or_else(|| vulcan_home().join("cortex.redb"))
}

fn format_datetime(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M UTC").to_string()
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}
