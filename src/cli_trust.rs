//! YYC-182: `vulcan trust why` — explain workspace trust
//! resolution. Reads the same config Vulcan reads at startup so
//! the answer matches what the agent will actually use.

use anyhow::Result;

use crate::cli::TrustSubcommand;
use crate::config::Config;

pub async fn run(cmd: TrustSubcommand) -> Result<()> {
    match cmd {
        TrustSubcommand::Why { path } => why(path),
    }
}

fn why(target: Option<std::path::PathBuf>) -> Result<()> {
    let cfg = Config::load()?;
    let resolved_target = match target {
        Some(p) => p,
        None => std::env::current_dir()?,
    };
    let profile = cfg.workspace_trust.resolve_for(&resolved_target);
    println!("Workspace: {}", resolved_target.display());
    println!("  level:               {}", profile.level.as_str());
    println!("  capability_profile:  {}", profile.capability_profile);
    println!("  allow_indexing:      {}", profile.allow_indexing);
    println!("  allow_persistence:   {}", profile.allow_persistence);
    println!("  reason:              {}", profile.reason);
    Ok(())
}
