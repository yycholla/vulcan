//! YYC-185: `vulcan policy simulate` CLI driver.

use anyhow::Result;

use crate::cli::PolicySubcommand;
use crate::config::Config;
use crate::policy::{default_tool_universe, render_markdown, simulate};

pub async fn run(cmd: PolicySubcommand) -> Result<()> {
    match cmd {
        PolicySubcommand::Simulate { path, profile } => {
            let workspace = match path {
                Some(p) => p,
                None => std::env::current_dir()?,
            };
            let config = Config::load()?;
            let universe = default_tool_universe();
            let sim = simulate(&config, &workspace, profile.as_deref(), &universe);
            print!("{}", render_markdown(&sim));
            Ok(())
        }
    }
}
