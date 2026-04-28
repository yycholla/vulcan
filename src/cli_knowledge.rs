//! YYC-194: `vulcan knowledge list` — display local knowledge
//! stores with size + last-modified.

use anyhow::{Result, anyhow};
use std::io::{self, BufRead as _, Write};

use crate::cli::KnowledgeSubcommand;
use crate::knowledge::{self, KnowledgeStoreInfo};

pub async fn run(cmd: KnowledgeSubcommand) -> Result<()> {
    match cmd {
        KnowledgeSubcommand::List => list(),
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
}
