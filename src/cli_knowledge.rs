//! YYC-194: `vulcan knowledge list` — display local knowledge
//! stores with size + last-modified.

use anyhow::Result;

use crate::cli::KnowledgeSubcommand;
use crate::knowledge;

pub async fn run(cmd: KnowledgeSubcommand) -> Result<()> {
    match cmd {
        KnowledgeSubcommand::List => list(),
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
