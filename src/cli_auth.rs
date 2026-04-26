//! `vulcan auth` — guided interactive provider setup (YYC-100).
//!
//! Pairs the YYC-98 preset catalog with `dialoguer` prompts: pick
//! provider → name → API key → model → confirm → write. Front-end
//! only; the actual writer is `cli_provider::add`.

use anyhow::{Context, Result, bail};
use clap::Args;
use dialoguer::{Confirm, FuzzySelect, Input, Password};
use std::io::IsTerminal;
use std::path::Path;

use crate::cli_provider::{AddArgs, Preset, add, lookup_preset, presets};

#[derive(Args, Debug)]
pub struct AuthArgs {
    /// Skip the picker by naming a preset directly (e.g. `vulcan auth openrouter`).
    pub preset: Option<String>,
    /// Jump straight to the custom (non-preset) flow: prompts for base
    /// URL, model, and API key without a preset shortcut.
    #[arg(long)]
    pub custom: bool,
}

pub async fn run(args: AuthArgs, dir: std::path::PathBuf) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!(
            "vulcan auth requires an interactive terminal. \
             For non-interactive flows use `vulcan provider add ...`."
        );
    }

    let theme = dialoguer_theme();

    if args.custom {
        return run_custom(&theme, &dir);
    }

    if let Some(key) = args.preset.as_deref() {
        let preset = lookup_preset(key)
            .with_context(|| format!("unknown preset '{key}'. See `vulcan provider presets`."))?;
        return run_preset(&theme, &dir, preset);
    }

    // No preset supplied → fuzzy picker.
    let presets = presets();
    let mut labels: Vec<String> = presets
        .iter()
        .map(|p| format!("{}  ({})", p.display, p.key))
        .collect();
    let custom_label = "Custom (enter base URL by hand)".to_string();
    labels.push(custom_label);

    let pick = FuzzySelect::with_theme(&theme)
        .with_prompt("Pick a provider")
        .items(&labels)
        .default(0)
        .interact()
        .context("provider picker cancelled")?;

    if pick == presets.len() {
        run_custom(&theme, &dir)
    } else {
        run_preset(&theme, &dir, &presets[pick])
    }
}

fn run_preset(theme: &dyn dialoguer::theme::Theme, dir: &Path, preset: &Preset) -> Result<()> {
    println!();
    println!("Selected: {} ({})", preset.display, preset.key);
    println!("  base_url     : {}", preset.base_url);
    println!("  default_model: {}", preset.model);
    println!("  auth         : {}", preset.auth_hint);
    if !preset.notes.is_empty() {
        println!("  notes        : {}", preset.notes);
    }
    println!();

    let name: String = Input::with_theme(theme)
        .with_prompt("Profile name")
        .default(preset.key.to_string())
        .validate_with(|input: &String| -> Result<(), &str> {
            if input.eq_ignore_ascii_case("default") {
                Err("'default' is reserved for the legacy [provider] block")
            } else if input.trim().is_empty() {
                Err("name required")
            } else {
                Ok(())
            }
        })
        .interact_text()?;

    let api_key: String = Password::with_theme(theme)
        .with_prompt(format!(
            "API key (Press Enter to skip and rely on env var: {})",
            preset.auth_hint
        ))
        .allow_empty_password(true)
        .interact()?;

    let model: String = Input::with_theme(theme)
        .with_prompt("Default model id")
        .default(preset.model.to_string())
        .interact_text()?;

    let force = if profile_exists(dir, &name)? {
        Confirm::with_theme(theme)
            .with_prompt(format!(
                "Profile '{name}' already exists in providers.toml. Overwrite?"
            ))
            .default(false)
            .interact()?
    } else {
        false
    };

    let confirm = Confirm::with_theme(theme)
        .with_prompt(format!(
            "Write [{}] to providers.toml (base_url={}, model={})?",
            name, preset.base_url, model
        ))
        .default(true)
        .interact()?;
    if !confirm {
        println!("Aborted — nothing written.");
        return Ok(());
    }

    let api_key_opt = if api_key.trim().is_empty() {
        None
    } else {
        Some(api_key)
    };
    add(
        AddArgs {
            name: name.clone(),
            preset: Some(preset.key.to_string()),
            base_url: None,
            model: Some(model),
            api_key: api_key_opt,
            max_context: None,
            disable_catalog: false,
            force,
        },
        dir,
    )?;
    println!();
    println!("Done. Switch into the new profile with `/provider {name}` in the TUI.");
    Ok(())
}

fn run_custom(theme: &dyn dialoguer::theme::Theme, dir: &Path) -> Result<()> {
    println!();
    println!("Custom provider — enter the OpenAI-compatible endpoint by hand.");
    println!();

    let name: String = Input::with_theme(theme)
        .with_prompt("Profile name")
        .validate_with(|input: &String| -> Result<(), &str> {
            if input.eq_ignore_ascii_case("default") {
                Err("'default' is reserved for the legacy [provider] block")
            } else if input.trim().is_empty() {
                Err("name required")
            } else {
                Ok(())
            }
        })
        .interact_text()?;

    let base_url: String = Input::with_theme(theme)
        .with_prompt("Base URL (must be OpenAI-compatible, e.g. https://example.com/v1)")
        .validate_with(|input: &String| -> Result<(), &str> {
            if input.starts_with("http://") || input.starts_with("https://") {
                Ok(())
            } else {
                Err("must start with http:// or https://")
            }
        })
        .interact_text()?;

    let model: String = Input::with_theme(theme)
        .with_prompt("Default model id")
        .interact_text()?;

    let api_key: String = Password::with_theme(theme)
        .with_prompt("API key (Enter to skip — auth then resolves via VULCAN_API_KEY)")
        .allow_empty_password(true)
        .interact()?;

    let disable_catalog = Confirm::with_theme(theme)
        .with_prompt("Skip the /models catalog fetch at startup? (yes for self-hosted endpoints)")
        .default(false)
        .interact()?;

    let force = if profile_exists(dir, &name)? {
        Confirm::with_theme(theme)
            .with_prompt(format!(
                "Profile '{name}' already exists in providers.toml. Overwrite?"
            ))
            .default(false)
            .interact()?
    } else {
        false
    };

    let confirm = Confirm::with_theme(theme)
        .with_prompt(format!(
            "Write [{}] to providers.toml (base_url={}, model={})?",
            name, base_url, model
        ))
        .default(true)
        .interact()?;
    if !confirm {
        println!("Aborted — nothing written.");
        return Ok(());
    }

    let api_key_opt = if api_key.trim().is_empty() {
        None
    } else {
        Some(api_key)
    };

    add(
        AddArgs {
            name: name.clone(),
            preset: None,
            base_url: Some(base_url),
            model: Some(model),
            api_key: api_key_opt,
            max_context: None,
            disable_catalog,
            force,
        },
        dir,
    )?;
    println!();
    println!("Done. Switch into the new profile with `/provider {name}` in the TUI.");
    Ok(())
}

fn profile_exists(dir: &Path, name: &str) -> Result<bool> {
    let cfg = crate::config::Config::load_from_dir(dir).unwrap_or_default();
    Ok(cfg.providers.contains_key(name))
}

fn dialoguer_theme() -> dialoguer::theme::ColorfulTheme {
    dialoguer::theme::ColorfulTheme::default()
}
