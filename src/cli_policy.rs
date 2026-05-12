//! YYC-185: `vulcan policy simulate` CLI driver.

use anyhow::{Result, bail};

use crate::cli::PolicySubcommand;
use crate::config::Config;
use crate::policy::{
    TrustOverride, default_tool_universe, render_dry_run_markdown, render_markdown, simulate,
    simulate_dry_run,
};
use crate::trust::TrustLevel;

pub async fn run(cmd: PolicySubcommand) -> Result<()> {
    match cmd {
        PolicySubcommand::Simulate {
            path,
            profile,
            trust_level,
            trust_profile,
        } => {
            let workspace = match path {
                Some(p) => p,
                None => std::env::current_dir()?,
            };
            let config = Config::load()?;
            let universe = default_tool_universe();
            let trust_override = match trust_level {
                Some(level) => Some(TrustOverride {
                    level: parse_trust_level(&level)?,
                    capability_profile: trust_profile,
                }),
                None => {
                    if trust_profile.is_some() {
                        bail!("--trust-profile requires --trust-level");
                    }
                    None
                }
            };
            if profile.is_some() || trust_override.is_some() {
                let dry_run = simulate_dry_run(
                    &config,
                    &workspace,
                    profile.as_deref(),
                    trust_override,
                    &universe,
                );
                print!("{}", render_dry_run_markdown(&dry_run));
            } else {
                let sim = simulate(&config, &workspace, None, &universe);
                print!("{}", render_markdown(&sim));
            }
            Ok(())
        }
    }
}

fn parse_trust_level(raw: &str) -> Result<TrustLevel> {
    match raw {
        "trusted" => Ok(TrustLevel::Trusted),
        "restricted" => Ok(TrustLevel::Restricted),
        "sensitive" => Ok(TrustLevel::Sensitive),
        "untrusted" => Ok(TrustLevel::Untrusted),
        other => bail!(
            "unknown trust level `{}` (expected trusted, restricted, sensitive, or untrusted)",
            other
        ),
    }
}
