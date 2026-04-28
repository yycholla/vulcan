//! YYC-219: `vulcan context-pack list` / `vulcan context-pack show <name>`.
//!
//! Surfaces the built-in context pack catalog (`crate::context_pack`)
//! so users can see what's available before adding `--context-pack`
//! to a `vulcan prompt` invocation.

use anyhow::Result;

use crate::cli::ContextPackSubcommand;
use crate::context_pack::{builtin_packs, lookup, render_pack_summary};

pub async fn run(cmd: ContextPackSubcommand) -> Result<()> {
    match cmd {
        ContextPackSubcommand::List => list(),
        ContextPackSubcommand::Show { name } => show(&name),
    }
}

fn list() -> Result<()> {
    let packs = builtin_packs();
    if packs.is_empty() {
        println!("(no packs)");
        return Ok(());
    }
    println!("{:<16} {}", "name", "description");
    for pack in &packs {
        println!("{:<16} {}", pack.name, pack.description);
    }
    Ok(())
}

fn show(name: &str) -> Result<()> {
    let pack = lookup(name)
        .ok_or_else(|| anyhow::anyhow!("context pack `{name}` not found. Run `vulcan context-pack list` to see available packs."))?;
    print!("{}", render_pack_summary(&pack));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn list_runs_without_error() {
        run(ContextPackSubcommand::List).await.unwrap();
    }

    #[tokio::test]
    async fn show_returns_error_for_unknown_pack() {
        let err = run(ContextPackSubcommand::Show {
            name: "definitely-not-a-real-pack-xyz".into(),
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn show_succeeds_for_builtin() {
        run(ContextPackSubcommand::Show {
            name: "gateway".into(),
        })
        .await
        .unwrap();
    }
}
