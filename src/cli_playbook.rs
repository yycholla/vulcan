//! YYC-220 / YYC-187: `vulcan playbook` CLI driver.

use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::cli::PlaybookSubcommand;
use crate::playbook::{
    EntryStatus, PlaybookEntry, PlaybookSection, PlaybookStore, SqlitePlaybookStore,
};

pub async fn run(cmd: PlaybookSubcommand) -> Result<()> {
    let store = SqlitePlaybookStore::try_new()?;
    let workspace = workspace_key()?;
    match cmd {
        PlaybookSubcommand::List { status } => list(&store, &workspace, status.as_deref()),
        PlaybookSubcommand::Show { id } => show(&store, &workspace, &id),
        PlaybookSubcommand::Accept { id } => accept(&store, &workspace, &id),
        PlaybookSubcommand::Remove { id } => remove(&store, &workspace, &id),
        PlaybookSubcommand::Import { path } => {
            let target = path.unwrap_or(std::env::current_dir()?);
            import(&store, &workspace, &target)
        }
    }
}

fn list(store: &SqlitePlaybookStore, workspace: &str, status_filter: Option<&str>) -> Result<()> {
    let entries = store.list_entries(workspace)?;
    let filter = match status_filter {
        Some("proposed") => Some(EntryStatus::Proposed),
        Some("accepted") => Some(EntryStatus::Accepted),
        Some(other) => return Err(anyhow!("unknown status `{other}`. Use proposed|accepted.")),
        None => None,
    };
    let filtered: Vec<&PlaybookEntry> = entries
        .iter()
        .filter(|e| filter.map_or(true, |f| e.status == f))
        .collect();
    if filtered.is_empty() {
        println!("(no entries)");
        return Ok(());
    }
    println!(
        "{:<10} {:<14} {:<10} {:<22} body",
        "id", "section", "status", "source"
    );
    for e in filtered {
        let id_short: String = e.id.to_string().chars().take(8).collect();
        let body_preview: String = e.body.replace('\n', " ").chars().take(60).collect();
        let status = match e.status {
            EntryStatus::Proposed => "proposed",
            EntryStatus::Accepted => "accepted",
        };
        println!(
            "{:<10} {:<14} {:<10} {:<22} {}",
            id_short,
            e.section.as_str(),
            status,
            shorten(&e.source, 20),
            body_preview
        );
    }
    Ok(())
}

fn show(store: &SqlitePlaybookStore, workspace: &str, raw_id: &str) -> Result<()> {
    let entry = resolve_entry(store, workspace, raw_id)?;
    println!("Playbook entry {}", entry.id);
    println!("  section: {}", entry.section.as_str());
    println!(
        "  status:  {}",
        match entry.status {
            EntryStatus::Proposed => "proposed",
            EntryStatus::Accepted => "accepted",
        }
    );
    println!("  source:  {}", entry.source);
    println!(
        "  created: {}",
        entry.created_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    println!("\n--- body ---");
    println!("{}", entry.body);
    Ok(())
}

fn accept(store: &SqlitePlaybookStore, workspace: &str, raw_id: &str) -> Result<()> {
    let entry = resolve_entry(store, workspace, raw_id)?;
    if entry.status == EntryStatus::Accepted {
        println!("(already accepted)");
        return Ok(());
    }
    let ok = store.accept_entry(workspace, entry.id)?;
    if ok {
        println!("accepted {}", entry.id);
    } else {
        return Err(anyhow!("entry {} not found", entry.id));
    }
    Ok(())
}

fn remove(store: &SqlitePlaybookStore, workspace: &str, raw_id: &str) -> Result<()> {
    let entry = resolve_entry(store, workspace, raw_id)?;
    let ok = store.remove_entry(workspace, entry.id)?;
    if ok {
        println!("removed {}", entry.id);
    } else {
        return Err(anyhow!("entry {} not found", entry.id));
    }
    Ok(())
}

fn import(store: &SqlitePlaybookStore, workspace: &str, root: &Path) -> Result<()> {
    let mut imported = 0usize;
    for filename in ["AGENTS.md", "CLAUDE.md", "README.md"] {
        let path = root.join(filename);
        if !path.exists() {
            continue;
        }
        let body = std::fs::read_to_string(&path)?;
        if body.trim().is_empty() {
            continue;
        }
        let section = match filename {
            "AGENTS.md" | "CLAUDE.md" => PlaybookSection::AgentBehavior,
            _ => PlaybookSection::Architecture,
        };
        let entry = PlaybookEntry::proposed(section, body, format!("imported from {filename}"));
        store.upsert_entry(workspace, &entry)?;
        println!("imported {filename} as proposed entry {}", entry.id);
        imported += 1;
    }
    if imported == 0 {
        println!(
            "(no AGENTS.md / CLAUDE.md / README.md found at {})",
            root.display()
        );
    } else {
        println!(
            "\n{imported} entry(ies) added as `proposed`. Run `vulcan playbook accept <id>` to enable."
        );
    }
    Ok(())
}

fn resolve_entry(
    store: &SqlitePlaybookStore,
    workspace: &str,
    raw_id: &str,
) -> Result<PlaybookEntry> {
    if let Ok(uuid) = Uuid::parse_str(raw_id) {
        let entries = store.list_entries(workspace)?;
        return entries
            .into_iter()
            .find(|e| e.id == uuid)
            .ok_or_else(|| anyhow!("entry {uuid} not found"));
    }
    let entries = store.list_entries(workspace)?;
    let matches: Vec<PlaybookEntry> = entries
        .into_iter()
        .filter(|e| e.id.to_string().starts_with(raw_id))
        .collect();
    match matches.len() {
        0 => Err(anyhow!("no entry matches `{raw_id}`")),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => Err(anyhow!(
            "id prefix `{raw_id}` matched {n} entries — supply more characters"
        )),
    }
}

/// Stable workspace key — canonical cwd path. Same shape used by
/// the rest of the codebase (code_graph / embeddings).
fn workspace_key() -> Result<String> {
    let cwd = std::env::current_dir()?;
    let canonical: PathBuf = cwd.canonicalize().unwrap_or(cwd);
    Ok(canonical.display().to_string())
}

fn shorten(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}
