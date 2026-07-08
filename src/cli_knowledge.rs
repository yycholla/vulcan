//! YYC-194: `vulcan knowledge` governance commands.

use anyhow::{Result, anyhow};
use std::io::{self, BufRead as _, Write};

use crate::cli::KnowledgeSubcommand;
use crate::context_pack::{self, ContextPack, ContextSource};
use crate::knowledge::{self, KnowledgeStoreInfo};

pub async fn run(cmd: KnowledgeSubcommand) -> Result<()> {
    match cmd {
        KnowledgeSubcommand::List => list(),
        KnowledgeSubcommand::Why {
            task,
            pack,
            source_id,
        } => why(&task, pack.as_deref(), source_id.as_deref()),
        KnowledgeSubcommand::Purge {
            kind,
            workspace,
            all,
            yes,
        } => purge(kind.as_deref(), workspace.as_deref(), all, yes),
    }
}

fn list() -> Result<()> {
    let stores = knowledge::discover()?;
    if stores.is_empty() {
        println!("No local knowledge stores found.");
        return Ok(());
    }
    println!(
        "{:<14} {:>10} {:<22} {:<28} path",
        "kind", "size", "modified", "workspace"
    );
    let mut total: u64 = 0;
    for s in &stores {
        let modified = s
            .modified
            .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "-".into());
        let ws = s.workspace_key.clone().unwrap_or_else(|| "-".into());
        let size = format_bytes(s.size_bytes);
        let path = s.path.display();
        let kind = s.kind.as_str();
        println!("{kind:<14} {size:>10} {modified:<22} {ws:<28} {path}");
        total += s.size_bytes;
    }
    println!();
    println!("{} stores · total {}", stores.len(), format_bytes(total));
    Ok(())
}

fn why(task: &str, pack: Option<&str>, source_id: Option<&str>) -> Result<()> {
    print!("{}", render_why(task, pack, source_id)?);
    Ok(())
}

fn render_why(task: &str, pack_filter: Option<&str>, source_id: Option<&str>) -> Result<String> {
    let task = task.trim();
    if task.is_empty() {
        return Err(anyhow!("task must not be empty"));
    }

    let packs = selected_packs(pack_filter)?;
    let task_terms = task_terms(task);
    let mut out = String::new();
    out.push_str(&format!("Task: {task}\n"));
    out.push_str("Explanation source: context-pack metadata\n\n");

    let mut rendered = 0usize;
    for pack in &packs {
        let mut lines = Vec::new();
        let include_all_pack_sources = pack_filter.is_some() || source_id.is_some();
        for source in &pack.sources {
            let id = source.short_label();
            if let Some(wanted) = source_id {
                if id != wanted {
                    continue;
                }
            }

            let matched_terms = matching_terms(&task_terms, pack, source, &id);
            if !include_all_pack_sources && matched_terms.is_empty() {
                continue;
            }

            let matches = if matched_terms.is_empty() {
                "none".to_string()
            } else {
                matched_terms.join(", ")
            };
            lines.push(format!(
                "- source-id: {id}\n  reason: {}\n  matched task terms: {matches}",
                source_reason(source)
            ));
        }

        if lines.is_empty() {
            continue;
        }

        out.push_str(&format!("## context-pack:{}\n", pack.name));
        out.push_str(&format!("description: {}\n", pack.description));
        out.push_str(&lines.join("\n"));
        out.push_str("\n\n");
        rendered += lines.len();
    }

    if rendered == 0 {
        if let Some(source_id) = source_id {
            let scope = pack_filter
                .map(|name| format!(" in context pack `{name}`"))
                .unwrap_or_default();
            return Err(anyhow!("source-id `{source_id}` not found{scope}"));
        }

        let available = packs
            .iter()
            .map(|pack| pack.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "No context-pack sources matched this task. Available packs: {available}\n"
        ));
    }

    Ok(out)
}

fn selected_packs(pack: Option<&str>) -> Result<Vec<ContextPack>> {
    match pack {
        Some(name) => context_pack::lookup(name)
            .map(|pack| vec![pack])
            .ok_or_else(|| anyhow!("context pack `{name}` not found")),
        None => Ok(context_pack::builtin_packs()),
    }
}

fn task_terms(task: &str) -> Vec<String> {
    task.split(|c: char| !c.is_alphanumeric())
        .filter_map(|term| {
            let term = term.trim().to_lowercase();
            (term.len() >= 3).then_some(term)
        })
        .collect()
}

fn matching_terms(
    task_terms: &[String],
    pack: &ContextPack,
    source: &ContextSource,
    source_id: &str,
) -> Vec<String> {
    let haystack = format!(
        "{} {} {} {}",
        pack.name,
        pack.description,
        source_id,
        source_reason(source)
    )
    .to_lowercase();

    let mut matched = task_terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    matched.sort();
    matched.dedup();
    matched
}

fn source_reason(source: &ContextSource) -> &str {
    match source {
        ContextSource::File { why, .. }
        | ContextSource::Doc { why, .. }
        | ContextSource::Run { why, .. }
        | ContextSource::Artifact { why, .. } => why,
        ContextSource::Note { text } => text,
    }
}

fn purge(kind: Option<&str>, workspace: Option<&str>, all: bool, skip_prompt: bool) -> Result<()> {
    if !all && kind.is_none() {
        return Err(anyhow!(
            "specify --kind <name>, --all, or both --kind + --workspace"
        ));
    }
    let stores = knowledge::discover()?;
    if stores.is_empty() {
        println!("No local knowledge stores found.");
        return Ok(());
    }
    let targets: Vec<KnowledgeStoreInfo> = stores
        .into_iter()
        .filter(|s| {
            if all {
                return true;
            }
            if let Some(k) = kind {
                if s.kind.as_str() != k {
                    return false;
                }
            }
            if let Some(w) = workspace {
                if s.workspace_key.as_deref() != Some(w) {
                    return false;
                }
            }
            true
        })
        .collect();
    if targets.is_empty() {
        println!("No stores matched.");
        return Ok(());
    }
    println!("About to purge {} store(s):", targets.len());
    let mut total: u64 = 0;
    for t in &targets {
        let ws = t.workspace_key.clone().unwrap_or_else(|| "-".into());
        println!(
            "  {}  {}  {}  {}",
            t.kind.as_str(),
            ws,
            format_bytes(t.size_bytes),
            t.path.display()
        );
        total += t.size_bytes;
    }
    println!("Total: {}", format_bytes(total));
    if !skip_prompt && !confirm("Proceed? Type `purge` to confirm: ", "purge")? {
        println!("Aborted.");
        return Ok(());
    }
    let mut freed: u64 = 0;
    let mut errors: Vec<String> = Vec::new();
    for t in &targets {
        match knowledge::purge(t) {
            Ok(n) => freed += n,
            Err(e) => errors.push(format!("{}: {e}", t.path.display())),
        }
    }
    println!("Purged {}.", format_bytes(freed));
    if !errors.is_empty() {
        eprintln!("Errors:");
        for e in &errors {
            eprintln!("  {e}");
        }
        return Err(anyhow!("{} purge failure(s)", errors.len()));
    }
    Ok(())
}

fn confirm(prompt: &str, expect: &str) -> Result<bool> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(prompt.as_bytes())?;
    stdout.flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim() == expect)
}

fn format_bytes(n: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if n >= GIB {
        format!("{:.1} GiB", n as f64 / GIB as f64)
    } else if n >= MIB {
        format!("{:.1} MiB", n as f64 / MIB as f64)
    } else if n >= KIB {
        format!("{:.1} KiB", n as f64 / KIB as f64)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_picks_appropriate_unit() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KiB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MiB");
        assert!(format_bytes(3 * 1024 * 1024 * 1024).starts_with("3.0 GiB"));
    }

    #[test]
    fn render_why_explains_filtered_source() {
        let report = render_why(
            "gateway queue lifecycle",
            Some("gateway"),
            Some("file:src/gateway/queue.rs"),
        )
        .unwrap();

        assert!(report.contains("Task: gateway queue lifecycle"));
        assert!(report.contains("## context-pack:gateway"));
        assert!(report.contains("source-id: file:src/gateway/queue.rs"));
        assert!(report.contains("reason: durable inbound/outbound queues"));
        assert!(report.contains("matched task terms: gateway, queue"));
    }

    #[test]
    fn render_why_fails_clear_for_unknown_source() {
        let err = render_why(
            "gateway queue lifecycle",
            Some("gateway"),
            Some("file:src/missing.rs"),
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("source-id `file:src/missing.rs` not found")
        );
    }

    #[test]
    fn render_why_lists_available_packs_when_no_source_matches() {
        let report = render_why("zzzzzzzz", None, None).unwrap();

        assert!(report.contains("No context-pack sources matched this task."));
        assert!(report.contains("Available packs:"));
    }
}
