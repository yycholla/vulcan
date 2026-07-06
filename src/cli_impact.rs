//! YYC-218 / YYC-189: `vulcan impact <file>` driver.

use anyhow::{Result, bail};
use std::path::Path;

use crate::artifact::{Artifact, ArtifactKind, SqliteArtifactStore};
use crate::cli::ImpactSubcommand;
use crate::impact::{generate_for_file, generate_for_symbol, generate_for_task, render_markdown};

pub async fn run(target: Option<&Path>, cmd: Option<&ImpactSubcommand>, save: bool) -> Result<()> {
    let workspace = std::env::current_dir()?;
    let (report, title) = match (target, cmd) {
        (Some(path), None) => (
            generate_for_file(&workspace, path)?,
            format!("Impact: {}", path.display()),
        ),
        (None, Some(ImpactSubcommand::File { target })) => (
            generate_for_file(&workspace, target)?,
            format!("Impact: {}", target.display()),
        ),
        (None, Some(ImpactSubcommand::Symbol { name })) => (
            generate_for_symbol(&workspace, name)?,
            format!("Impact: symbol {name}"),
        ),
        (None, Some(ImpactSubcommand::Task { text })) => (
            generate_for_task(&workspace, text)?,
            "Impact: task".to_string(),
        ),
        (Some(_), Some(_)) => {
            bail!("pass either `vulcan impact <file>` or an impact subcommand, not both")
        }
        (None, None) => bail!(
            "missing impact target; use `vulcan impact <file>`, `vulcan impact symbol <name>`, or `vulcan impact task <text>`"
        ),
    };
    let md = render_markdown(&report);
    print!("{md}");
    if save {
        persist_report(md, title).await?;
    }
    Ok(())
}

async fn persist_report(md: String, title: String) -> Result<()> {
    match SqliteArtifactStore::try_new() {
        Ok(store) => {
            let art = Artifact::inline_text(ArtifactKind::Report, md)
                .with_source("impact")
                .with_title(title);
            let id = art.id;
            if let Err(e) = crate::artifact::ArtifactStore::create(&store, &art).await {
                eprintln!("artifact persist failed: {e}");
            } else {
                eprintln!("impact report persisted as artifact {id}");
            }
        }
        Err(e) => eprintln!("artifact store unavailable: {e}"),
    }
    Ok(())
}
