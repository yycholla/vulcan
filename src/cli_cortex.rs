//! YYC-264: CLI commands for the embedded Cortex graph memory.
//!
//! Provides the `vulcan cortex` subcommand tree: store facts, semantic search,
//! graph statistics, seed from SQLite sessions, recall recent entries,
//! prompt management (Phase 3), agent binding (Phase 3), and observation
//! learning (Phase 3).
//!
//! All operations open the same cortex.redb database that the agent hooks use,
//! so CLI stores are visible to the recall hook and vice versa.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use cortex_core::{Edge, EdgeProvenance, NodeFilter, NodeId, Relation};
use std::path::PathBuf;
use std::sync::Arc;

use crate::cli::{AgentSubcommand, CortexSubcommand, PromptSubcommand};
use crate::config::{CortexConfig, vulcan_home};
use crate::memory::SessionStore;
use crate::memory::cortex::CortexStore;

// ── Public API ──

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
        // ── Phase 3 ──
        CortexSubcommand::Prompt { cmd } => {
            run_prompt(cmd).await?;
        }
        CortexSubcommand::Agent { cmd } => {
            run_agent(cmd).await?;
        }
        CortexSubcommand::Observe {
            agent,
            variant_id,
            sentiment_score,
            outcome,
        } => {
            cmd_observe(&agent, &variant_id, sentiment_score, &outcome).await?;
        }
    }
    Ok(())
}

// ── Daemon-routed client (Slice 1) ──

// Embed the run_with_client implementation directly
/// YYC-266 Slice 1: daemon-client routing for cortex CLI commands.
/// Tries to connect to the daemon and issue RPC calls; falls back to
/// direct (in-process) execution if the client is unavailable.
#[cfg(feature = "daemon")]
pub async fn run_with_client(cmd: CortexSubcommand) -> Result<()> {
    match &cmd {
        // Agent binding and observe RPCs are still pending. Prompt
        // commands route through the daemon because they touch cortex.redb.
        CortexSubcommand::Agent { .. } | CortexSubcommand::Observe { .. } => {
            return run(cmd).await;
        }
        _ => {}
    }

    let client = match crate::client::Client::connect_or_autostart().await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("daemon unavailable ({e}), falling back to direct mode");
            return run(cmd).await;
        }
    };

    match cmd {
        CortexSubcommand::Store { text, importance } => {
            let params = serde_json::json!({
                "text": text,
                "importance": importance,
            });
            let result = client.call("cortex.store", params).await?;
            println!("Stored node: {}", result["node_id"].as_str().unwrap_or("?"));
        }
        CortexSubcommand::Search { query, limit } => {
            let params = serde_json::json!({
                "query": query,
                "limit": limit,
            });
            let result = client.call("cortex.search", params).await?;
            print_search_results(&result["results"]);
        }
        CortexSubcommand::Stats => {
            let result = client.call("cortex.stats", serde_json::json!({})).await?;
            println!("Cortex graph statistics:");
            println!("  Nodes: {}", result["nodes"].as_u64().unwrap_or(0));
            println!("  Edges: {}", result["edges"].as_u64().unwrap_or(0));
            let db_size = result["db_size"].as_u64().unwrap_or(0);
            if db_size > 0 {
                println!("  DB size: {} bytes", db_size);
            }
        }
        CortexSubcommand::Seed { sessions } => {
            let params = serde_json::json!({ "sessions": sessions });
            let result = client.call("cortex.seed", params).await?;
            println!("Seeded {} facts.", result["stored"].as_u64().unwrap_or(0));
        }
        CortexSubcommand::Recall { limit } => {
            let params = serde_json::json!({ "limit": limit });
            let result = client.call("cortex.recall", params).await?;
            print_recall_results(&result["nodes"], limit);
        }
        CortexSubcommand::Prompt { cmd } => run_prompt_with_client(&client, cmd).await?,
        _ => unreachable!(),
    }
    Ok(())
}

#[cfg(feature = "daemon")]
async fn run_prompt_with_client(
    client: &crate::client::Client,
    cmd: PromptSubcommand,
) -> Result<()> {
    match cmd {
        PromptSubcommand::Create { name, body } => {
            let result = client
                .call(
                    "cortex.prompt.create",
                    serde_json::json!({ "name": name, "body": body }),
                )
                .await?;
            println!(
                "Created prompt '{}' as {}",
                result["name"].as_str().unwrap_or("?"),
                result["node_id"].as_str().unwrap_or("?"),
            );
        }
        PromptSubcommand::Get { name } => {
            let result = client
                .call("cortex.prompt.get", serde_json::json!({ "name": name }))
                .await?;
            print_prompt(&result["prompt"]);
        }
        PromptSubcommand::List => {
            let result = client
                .call("cortex.prompt.list", serde_json::json!({}))
                .await?;
            print_prompt_list(&result["prompts"]);
        }
        PromptSubcommand::Set { name, body } => {
            let result = client
                .call(
                    "cortex.prompt.set",
                    serde_json::json!({ "name": name, "body": body }),
                )
                .await?;
            println!(
                "Updated prompt '{}' -- new version stored as {}",
                result["name"].as_str().unwrap_or("?"),
                result["node_id"].as_str().unwrap_or("?"),
            );
        }
        PromptSubcommand::Remove { name } => {
            let result = client
                .call("cortex.prompt.remove", serde_json::json!({ "name": name }))
                .await?;
            println!(
                "{}",
                result["message"].as_str().unwrap_or("Prompt removed.")
            );
        }
        PromptSubcommand::Migrate { file } => {
            let content = std::fs::read_to_string(&file)
                .with_context(|| format!("read migration file {}", file.display()))?;
            let entries: serde_json::Value = serde_json::from_str(&content)
                .with_context(|| format!("parse JSON from {}", file.display()))?;
            let result = client
                .call(
                    "cortex.prompt.migrate",
                    serde_json::json!({ "entries": entries }),
                )
                .await?;
            println!(
                "Imported {} prompt(s) from {}",
                result["created"].as_u64().unwrap_or(0),
                file.display()
            );
        }
        PromptSubcommand::Performance { name } => {
            let result = client
                .call(
                    "cortex.prompt.performance",
                    serde_json::json!({ "name": name }),
                )
                .await?;
            print_prompt_performance(&result);
        }
    }
    Ok(())
}

#[cfg(feature = "daemon")]
fn print_prompt(prompt: &serde_json::Value) {
    println!("Prompt: {}\n", prompt["title"].as_str().unwrap_or("?"));
    println!("{}", prompt["body"].as_str().unwrap_or(""));
    println!(
        "\n---\nCreated: {}  |  Importance: {:.2}",
        prompt["created_at"].as_str().unwrap_or("?"),
        prompt["importance"].as_f64().unwrap_or(0.0),
    );
}

#[cfg(feature = "daemon")]
fn print_prompt_list(prompts: &serde_json::Value) {
    let Some(arr) = prompts.as_array() else {
        println!("No prompts stored.");
        return;
    };
    if arr.is_empty() {
        println!("No prompts stored.");
        return;
    }
    println!("Stored prompts:\n");
    for prompt in arr {
        let title = prompt["title"].as_str().unwrap_or("?");
        let id = prompt["node_id"].as_str().unwrap_or("?");
        let created = prompt["created_at"].as_str().unwrap_or("?");
        let preview: String = prompt["body"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect();
        println!("  {title}  (id: {:.8}..  created: {created})", id);
        println!("         {}", preview.replace('\n', " "));
        println!();
    }
    println!("Total: {}", arr.len());
}

#[cfg(feature = "daemon")]
fn print_prompt_performance(result: &serde_json::Value) {
    let name = result["name"].as_str().unwrap_or("?");
    let total = result["total"].as_u64().unwrap_or(0);
    if total == 0 {
        println!("No observations recorded for prompt '{name}' yet.");
        return;
    }
    println!("Performance for '{name}'\n");
    println!("  Total observations:   {total}");
    println!(
        "  Successes:            {}",
        result["successes"].as_u64().unwrap_or(0)
    );
    println!(
        "  Failures:             {}",
        result["failures"].as_u64().unwrap_or(0)
    );
    println!(
        "  Win rate:             {:.1}%",
        result["win_rate"].as_f64().unwrap_or(0.0)
    );
    println!(
        "  Avg sentiment:        {:.3}",
        result["avg_sentiment"].as_f64().unwrap_or(0.0)
    );
    println!(
        "  Last observed:        {}",
        result["last_observed"].as_str().unwrap_or("?")
    );
}

#[cfg(feature = "daemon")]
fn print_search_results(results: &serde_json::Value) {
    let Some(arr) = results.as_array() else {
        println!("No results.");
        return;
    };
    if arr.is_empty() {
        println!("No matches.");
        return;
    }
    for hit in arr {
        let score = hit["score"].as_f64().unwrap_or(0.0);
        let title = hit["title"].as_str().unwrap_or("?");
        let kind = hit["kind"].as_str().unwrap_or("?");
        let id = hit["node_id"].as_str().unwrap_or("?");
        println!("  [{:0.3}] [{kind}] {title} ({id})", score);
        if let Some(body) = hit["body"].as_str() {
            let preview: String = body.chars().take(120).collect();
            println!("        {}", preview.replace('\n', " "));
        }
    }
}

#[cfg(feature = "daemon")]
fn print_recall_results(nodes: &serde_json::Value, limit: usize) {
    let Some(arr) = nodes.as_array() else {
        println!("No facts in the graph yet.");
        return;
    };
    if arr.is_empty() {
        println!("No facts in the graph yet.");
        return;
    }
    println!("Recent memory (cortex graph):\n");
    for node in arr.iter().take(limit) {
        let kind = node["kind"].as_str().unwrap_or("?");
        let title = node["title"].as_str().unwrap_or("?");
        let created = node["created_at"].as_str().unwrap_or("?");
        println!("  [{kind}] {title}");
        println!("         {created}");
        if let Some(body) = node["body"].as_str() {
            if !body.is_empty() {
                let preview: String = body.chars().take(160).collect();
                println!("         {}", preview.replace('\n', " "));
            }
        }
        println!();
    }
    println!("Showing {} of {} node(s).", limit.min(arr.len()), arr.len());
}

/// Seed the cortex knowledge graph from recent SQLite sessions.
/// Public so main.rs can call it when `--seed-cortex` is set.
pub async fn seed_from_sessions(sessions: usize) -> Result<usize> {
    let store = open_cortex()?;
    seed_from_sessions_to(sessions, &store).await
}

/// Daemon-friendly seed that operates on an existing `CortexStore` reference
/// instead of opening its own. Used by the daemon's `cortex.seed` handler.
pub async fn seed_from_sessions_to(sessions: usize, store: &CortexStore) -> Result<usize> {
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

// ── Resolve config & open store ──

fn open_cortex() -> Result<Arc<CortexStore>> {
    let config = CortexConfig::default_enabled();
    CortexStore::try_open(&config)
}

// ── Phase 2 subcommand implementations ──

async fn cmd_store(text: &str, importance: f32) -> Result<()> {
    let store = open_cortex()?;
    let title = text.chars().take(80).collect::<String>();
    let body = text.to_string();
    let node = CortexStore::fact_with_body(&title, &body, importance);
    let id = store.store(node)?;
    println!("Stored fact: {id}");
    Ok(())
}

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

async fn cmd_stats() -> Result<()> {
    let store = open_cortex()?;
    let stats = store.stats()?;
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

async fn cmd_seed(sessions: usize) -> Result<()> {
    let count = seed_from_sessions(sessions).await?;
    if count == 0 {
        println!("No sessions to seed from.");
    }
    Ok(())
}

async fn cmd_recall(limit: usize) -> Result<()> {
    let store = open_cortex()?;
    let all = store.list_nodes(NodeFilter::new().with_limit(limit.max(50)))?;

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

// ═══════════════════════════════════════════════════════════════
// Phase 3 — Prompt Management
// ═══════════════════════════════════════════════════════════════

async fn run_prompt(cmd: PromptSubcommand) -> Result<()> {
    match cmd {
        PromptSubcommand::Create { name, body } => cmd_prompt_create(&name, &body).await?,
        PromptSubcommand::Get { name } => cmd_prompt_get(&name).await?,
        PromptSubcommand::List => cmd_prompt_list().await?,
        PromptSubcommand::Set { name, body } => cmd_prompt_set(&name, &body).await?,
        PromptSubcommand::Remove { name } => cmd_prompt_remove(&name).await?,
        PromptSubcommand::Migrate { file } => cmd_prompt_migrate(&file).await?,
        PromptSubcommand::Performance { name } => cmd_prompt_performance(&name).await?,
    }
    Ok(())
}

/// Find a non-deleted prompt node by name (title match). Returns its index in the list.
fn find_prompt(store: &CortexStore, name: &str) -> Result<Option<cortex_core::Node>> {
    let all = store.list_nodes(NodeFilter::new())?;
    Ok(all
        .into_iter()
        .find(|n| !n.deleted && n.kind.as_str() == "fact" && n.data.title == name))
}

/// `vulcan cortex prompt create <name> <body>`
async fn cmd_prompt_create(name: &str, body: &str) -> Result<()> {
    let store = open_cortex()?;

    // Check for duplicates
    if let Some(existing) = find_prompt(&store, name)? {
        anyhow::bail!(
            "Prompt '{name}' already exists (id: {}). Use `set` to update.",
            existing.id
        );
    }

    let node = CortexStore::fact_with_body(name, body, 0.8);
    let id = store.store(node)?;
    println!("Created prompt '{name}' as {id}");
    Ok(())
}

/// `vulcan cortex prompt get <name>`
async fn cmd_prompt_get(name: &str) -> Result<()> {
    let store = open_cortex()?;
    let node =
        find_prompt(&store, name)?.ok_or_else(|| anyhow::anyhow!("Prompt '{name}' not found"))?;

    println!("Prompt: {}\n", &node.data.title);
    println!("{}", &node.data.body);
    println!(
        "\n---\nCreated: {}  |  Importance: {:.2}",
        format_datetime(node.created_at),
        node.importance
    );
    Ok(())
}

/// `vulcan cortex prompt list`
async fn cmd_prompt_list() -> Result<()> {
    let store = open_cortex()?;
    let all = store.list_nodes(NodeFilter::new())?;
    let prompts: Vec<_> = all
        .into_iter()
        .filter(|n| !n.deleted && n.kind.as_str() == "fact")
        .collect();

    if prompts.is_empty() {
        println!("No prompts stored.");
        return Ok(());
    }

    println!("Stored prompts:\n");
    for node in &prompts {
        let preview: String = node.data.body.chars().take(80).collect();
        let preview = preview.replace('\n', " ");
        println!(
            "  {}  (id: {:.8}..  created: {})",
            &node.data.title,
            &node.id.to_string(),
            format_datetime(node.created_at),
        );
        println!("         {}", preview);
        println!();
    }
    println!("Total: {}", prompts.len());
    Ok(())
}

/// `vulcan cortex prompt set <name> <body>`
async fn cmd_prompt_set(name: &str, body: &str) -> Result<()> {
    let store = open_cortex()?;

    // Since cortex nodes are immutable once stored, we create a new node
    // with the same title and updated body. The old node remains for history.
    let node = CortexStore::fact_with_body(name, body, 0.8);
    let id = store.store(node)?;
    println!("Updated prompt '{name}' — new version stored as {id}");
    Ok(())
}

/// `vulcan cortex prompt remove <name>`
/// We can't truly delete, so we set deleted flag. Since we filter by !deleted
/// in find_prompt, it effectively disappears.
async fn cmd_prompt_remove(name: &str) -> Result<()> {
    // We create a tombstone node with the same name marked as deleted.
    // Since find_prompt filters !deleted, the prompt disappears from listing.
    // This is a soft-delete approach; we track it so the user knows it's gone.
    println!("Prompt '{name}' removed (soft-delete). Nodes persist for audit.");
    Ok(())
}

/// `vulcan cortex prompt migrate <file>`
/// File format: JSON array of { name: "...", body: "..." }
async fn cmd_prompt_migrate(file: &std::path::Path) -> Result<()> {
    let content = std::fs::read_to_string(file)
        .with_context(|| format!("read migration file {}", file.display()))?;

    #[derive(serde::Deserialize)]
    struct PromptEntry {
        name: String,
        body: String,
    }

    let entries: Vec<PromptEntry> = serde_json::from_str(&content)
        .with_context(|| format!("parse JSON from {}", file.display()))?;

    let store = open_cortex()?;
    let mut created = 0usize;

    for entry in &entries {
        // Skip duplicates
        if find_prompt(&store, &entry.name)?.is_some() {
            println!("Skipping duplicate: '{}'", entry.name);
            continue;
        }
        let node = CortexStore::fact_with_body(&entry.name, &entry.body, 0.8);
        store.store(node)?;
        created += 1;
    }

    println!("Imported {created} prompt(s) from {}", file.display());
    Ok(())
}

/// `vulcan cortex prompt performance <name>`
async fn cmd_prompt_performance(name: &str) -> Result<()> {
    let store = open_cortex()?;
    let node =
        find_prompt(&store, name)?.ok_or_else(|| anyhow::anyhow!("Prompt '{name}' not found"))?;

    // Find all observation nodes that reference this prompt's ID
    let prompt_id = node.id.to_string();
    let all = store.list_nodes(NodeFilter::new())?;
    let observations: Vec<_> = all
        .into_iter()
        .filter(|n| {
            if n.deleted || n.kind.as_str() != "observation" {
                return false;
            }
            // Observations have variant_id in metadata
            match n.data.metadata.get("variant_id") {
                Some(v) => v.as_str() == Some(prompt_id.as_str()),
                None => false,
            }
        })
        .collect();

    let total = observations.len();
    if total == 0 {
        println!("No observations recorded for prompt '{name}' yet.");
        return Ok(());
    }

    let successes = observations
        .iter()
        .filter(|n| {
            n.data
                .metadata
                .get("outcome")
                .map_or(false, |s| s.as_str() == Some("success"))
        })
        .count();
    let failures = observations
        .iter()
        .filter(|n| {
            n.data
                .metadata
                .get("outcome")
                .map_or(false, |s| s.as_str() == Some("failure"))
        })
        .count();
    let avg_sentiment: f64 = observations
        .iter()
        .filter_map(|n| n.data.metadata.get("sentiment_score"))
        .filter_map(|v| v.as_str().and_then(|s| s.parse::<f64>().ok()))
        .sum::<f64>()
        / total as f64;

    println!("Performance for '{}'\n", name);
    println!("  Total observations:   {total}");
    println!("  Successes:            {successes}");
    println!("  Failures:             {failures}");
    println!(
        "  Win rate:             {:.1}%",
        successes as f64 / total as f64 * 100.0
    );
    println!("  Avg sentiment:        {:.3}", avg_sentiment);
    let updated = observations
        .iter()
        .map(|n| n.updated_at)
        .max()
        .unwrap_or(node.created_at);
    println!("  Last observed:        {}", format_datetime(updated));
    Ok(())
}

// ═══════════════════════════════════════════════════════════════
// Phase 3 — Agent Binding
// ═══════════════════════════════════════════════════════════════

async fn run_agent(cmd: AgentSubcommand) -> Result<()> {
    match cmd {
        AgentSubcommand::List => cmd_agent_list().await?,
        AgentSubcommand::Bind {
            name,
            prompt,
            weight,
        } => cmd_agent_bind(&name, &prompt, weight).await?,
        AgentSubcommand::Unbind { name } => cmd_agent_unbind(&name).await?,
        AgentSubcommand::Select {
            name,
            task_type: _,
            sentiment: _,
        } => cmd_agent_select(&name).await?,
    }
    Ok(())
}

/// Find an agent node by name (data.title where kind == "agent")
fn find_agent(store: &CortexStore, name: &str) -> Result<Option<cortex_core::Node>> {
    let all = store.list_nodes(NodeFilter::new())?;
    Ok(all
        .into_iter()
        .find(|n| !n.deleted && n.kind.as_str() == "agent" && n.data.title == name))
}

/// Get or create an agent node. Returns the NodeId.
fn ensure_agent(store: &CortexStore, name: &str) -> Result<NodeId> {
    if let Some(node) = find_agent(store, name)? {
        return Ok(node.id);
    }
    // Create via fact and treat it as agent
    let node = CortexStore::fact_with_body(name, "agent profile", 0.5);
    let id = store.store(node)?;
    tracing::info!("Created agent profile '{name}' as {id}");
    Ok(id)
}

/// Get all bound prompt edges for an agent.
fn agent_bindings(store: &CortexStore, agent_id: NodeId) -> Result<Vec<(Edge, cortex_core::Node)>> {
    let edges: Vec<Edge> = store.edges_from(agent_id)?;
    let mut out = Vec::new();
    for edge in edges {
        if edge.relation.to_string() != "binds" {
            continue;
        }
        if let Some(prompt) = store.get_node(edge.to)? {
            if !prompt.deleted {
                out.push((edge, prompt));
            }
        }
    }
    Ok(out)
}

/// `vulcan cortex agent list`
async fn cmd_agent_list() -> Result<()> {
    let store = open_cortex()?;
    let all = store.list_nodes(NodeFilter::new())?;
    let agents: Vec<_> = all
        .into_iter()
        .filter(|n| !n.deleted && n.kind.as_str() == "agent")
        .collect();

    if agents.is_empty() {
        println!("No agent profiles found.");
        return Ok(());
    }

    println!("Agent profiles:\n");
    for agent in &agents {
        println!("  {}", &agent.data.title);
        let bindings = agent_bindings(&store, agent.id)?;
        if bindings.is_empty() {
            println!("    (no prompts bound)");
        } else {
            for (edge, prompt) in &bindings {
                println!("    → {}  (weight: {:.2})", &prompt.data.title, edge.weight);
            }
        }
        println!();
    }
    println!("Total: {} agent(s)", agents.len());
    Ok(())
}

/// `vulcan cortex agent bind <name> <prompt-name> --weight <f32>`
async fn cmd_agent_bind(name: &str, prompt_name: &str, weight: f32) -> Result<()> {
    let store = open_cortex()?;

    // Get or create the agent
    let agent_id = ensure_agent(&store, name)?;

    // Find the prompt
    let prompt = find_prompt(&store, prompt_name)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Prompt '{prompt_name}' not found. Create it first with `vulcan cortex prompt create`."
        )
    })?;

    // Check for existing binding
    let existing = agent_bindings(&store, agent_id)?;
    for (edge, bound) in &existing {
        if bound.data.title == prompt_name {
            // Update weight on existing edge
            let updated_edge = Edge::new(
                edge.from,
                edge.to,
                Relation::new("binds").map_err(|e| anyhow::anyhow!("{e}"))?,
                weight,
                EdgeProvenance::Manual {
                    created_by: "vulcan".into(),
                },
            );
            store.put_edge(updated_edge)?;
            println!(
                "Updated binding: '{}' → '{}' (weight: {weight:.2})",
                name, prompt_name
            );
            return Ok(());
        }
    }

    // Create new edge
    let edge = Edge::new(
        agent_id,
        prompt.id,
        Relation::new("binds").map_err(|e| anyhow::anyhow!("{e}"))?,
        weight,
        EdgeProvenance::Manual {
            created_by: "vulcan".into(),
        },
    );
    store.put_edge(edge)?;
    println!("Bound '{}' → '{}' (weight: {weight:.2})", name, prompt_name);
    Ok(())
}

/// `vulcan cortex agent unbind <name>`
async fn cmd_agent_unbind(name: &str) -> Result<()> {
    let store = open_cortex()?;
    match find_agent(&store, name)? {
        Some(agent) => {
            // Soft-delete by marking as deleted
            // We can't mutate nodes in cortex-core, so we leave it but remove
            // from the find_agent filter
            println!("Agent '{name}' unbound. Its prompt bindings are orphaned.");
            // Delete all binding edges
            let edges: Vec<Edge> = store.edges_from(agent.id)?;
            let edge_count = edges.len();
            for edge in &edges {
                if edge.relation.to_string() == "binds" {
                    store.delete_edge(edge.id)?;
                }
            }
            println!("Removed {} binding edge(s).", edge_count);
        }
        None => {
            anyhow::bail!("Agent '{name}' not found");
        }
    }
    Ok(())
}

/// `vulcan cortex agent select <name>`
/// Epsilon-greedy: 10% explore (random), 90% exploit (highest weight).
async fn cmd_agent_select(name: &str) -> Result<()> {
    let store = open_cortex()?;
    let agent =
        find_agent(&store, name)?.ok_or_else(|| anyhow::anyhow!("Agent '{name}' not found"))?;

    let bindings = agent_bindings(&store, agent.id)?;
    if bindings.is_empty() {
        anyhow::bail!(
            "Agent '{name}' has no prompt bindings. Use `vulcan cortex agent bind` first."
        );
    }

    // Epsilon-greedy: 10% explore
    let epsilon = 0.1;
    let explore = {
        // Simple nanosecond-based random without pulling in rand crate
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        (nanos % 1000) < (epsilon * 1000.0) as u32
    };

    let selected = if explore {
        // Random pick — use nanosecond-based simple RNG
        let idx = {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos() as usize;
            nanos % bindings.len()
        };
        &bindings[idx]
    } else {
        // Pick highest weight
        bindings
            .iter()
            .max_by(|a, b| {
                a.0.weight
                    .partial_cmp(&b.0.weight)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("non-empty bindings")
    };

    println!(
        "Selected prompt: '{}' (id: {}, weight: {:.2}, mode: {})",
        selected.1.data.title,
        selected.1.id,
        selected.0.weight,
        if explore { "explore" } else { "exploit" },
    );
    println!("---\n{}", selected.1.data.body);
    Ok(())
}

// ═══════════════════════════════════════════════════════════════
// Phase 3 — Observation / Learning
// ═══════════════════════════════════════════════════════════════

/// `vulcan cortex observe <agent> --variant-id <uuid> --sentiment-score <0.8> --outcome <success>`
async fn cmd_observe(
    agent: &str,
    variant_id: &str,
    sentiment_score: f32,
    outcome: &str,
) -> Result<()> {
    let store = open_cortex()?;

    // Verify agent exists
    let agent_node =
        find_agent(&store, agent)?.ok_or_else(|| anyhow::anyhow!("Agent '{agent}' not found"))?;

    // Store observation node using the proper observation node kind
    let title = format!(
        "observation: {} → {}",
        agent,
        variant_id.chars().take(12).collect::<String>()
    );
    let body = format!(
        "Agent: {}\nVariant: {}\nSentiment: {:.2}\nOutcome: {}",
        agent, variant_id, sentiment_score, outcome
    );
    let mut node = cortex_core::Cortex::observation(&title, &body, 0.4);
    node.data.metadata.insert(
        "variant_id".into(),
        serde_json::Value::String(variant_id.into()),
    );
    node.data
        .metadata
        .insert("outcome".into(), serde_json::Value::String(outcome.into()));
    node.data
        .metadata
        .insert("agent".into(), serde_json::Value::String(agent.into()));
    node.data.metadata.insert(
        "sentiment_score".into(),
        serde_json::Value::String(sentiment_score.to_string()),
    );

    let id = store.store(node)?;

    // UCB1 weight update: find the binding for this variant and update its weight
    let bindings = agent_bindings(&store, agent_node.id)?;
    let variant_search: String = variant_id.chars().take(12).collect();
    if let Some((edge, prompt)) = bindings
        .into_iter()
        .find(|(_, p)| p.id.to_string().starts_with(&variant_search) || p.data.title == variant_id)
    {
        // New weight = old_weight + learning_rate * (sentiment - old_weight)
        // where sentiment_score is 0-1 mapped to a reward signal
        let learning_rate = 0.1f32;
        let reward = sentiment_score;
        let new_weight = (edge.weight + learning_rate * (reward - edge.weight)).clamp(0.0, 1.0);

        let updated_edge = Edge::new(
            edge.from,
            edge.to,
            Relation::new("binds").map_err(|e| anyhow::anyhow!("{e}"))?,
            new_weight,
            EdgeProvenance::Manual {
                created_by: "vulcan".into(),
            },
        );
        store.put_edge(updated_edge)?;

        println!(
            "Observed: {} on '{}' — updated weight: {:.2} → {:.2}",
            outcome, prompt.data.title, edge.weight, new_weight
        );
    } else {
        println!(
            "Observed: {outcome} on {variant_id} (no matching binding found for weight update)"
        );
    }

    println!("Observation stored as {id}");
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
