//! `vulcan provider …` subcommand: manage `[<name>]` blocks in
//! `~/.vulcan/providers.toml` without hand-editing TOML (YYC-98).
//!
//! Curated preset table covers the common providers (OpenRouter,
//! OpenAI, Anthropic, etc.) so a one-liner is enough to wire one with
//! sane base URL + default model. `toml_edit` preserves the user's
//! existing comments and key ordering across writes.
//!
//! Writes target the providers fragment file from YYC-99; the legacy
//! monolithic `config.toml` is never touched.

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use owo_colors::OwoColorize;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, value};

#[derive(Subcommand, Debug)]
pub enum ProviderCommand {
    /// Print every named profile plus the legacy `[provider]` block.
    List,
    /// Print the curated preset catalog.
    Presets,
    /// Write a new `[<name>]` block in providers.toml.
    Add(AddArgs),
    /// Delete a named `[<name>]` block from providers.toml.
    Remove(RemoveArgs),
    /// YYC-240 (YYC-238 PR-2): persist `active_profile = "<name>"`
    /// in config.toml so both TUI and gateway boot against this
    /// profile. `--clear` removes the override.
    Use(UseArgs),
}

#[derive(Args, Debug)]
pub struct UseArgs {
    /// Profile name to activate. Must already exist in
    /// providers.toml. Conflicts with `--clear`.
    pub name: Option<String>,
    /// Remove the persisted `active_profile` so the legacy
    /// `[provider]` block becomes the active one again.
    #[arg(long)]
    pub clear: bool,
}

#[derive(Args, Debug)]
pub struct AddArgs {
    /// Profile name (used as `[<name>]` and `/provider <name>`).
    pub name: String,
    /// Curated preset to base the profile on (see `vulcan provider presets`).
    #[arg(long)]
    pub preset: Option<String>,
    /// Override or set the OpenAI-compatible base URL.
    #[arg(long)]
    pub base_url: Option<String>,
    /// Override or set the default model id.
    #[arg(long)]
    pub model: Option<String>,
    /// Inline API key. Defaults to leaving it blank — auth then resolves
    /// via `VULCAN_API_KEY` or the per-provider env var the preset hints at.
    #[arg(long)]
    pub api_key: Option<String>,
    /// Override the default 128k context window.
    #[arg(long)]
    pub max_context: Option<usize>,
    /// Skip the `/models` catalog fetch at startup. Useful for
    /// self-hosted endpoints (Ollama, vLLM).
    #[arg(long)]
    pub disable_catalog: bool,
    /// Overwrite an existing profile with the same name without erroring.
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct RemoveArgs {
    /// Profile name to delete. `default` is reserved for the legacy
    /// `[provider]` block in config.toml and rejected here.
    pub name: String,
}

/// One curated provider preset surfaced by `vulcan provider presets`.
#[derive(Clone, Debug)]
pub struct Preset {
    pub key: &'static str,
    pub display: &'static str,
    pub base_url: &'static str,
    pub model: &'static str,
    pub auth_hint: &'static str,
    pub disable_catalog: bool,
    pub notes: &'static str,
}

pub fn presets() -> &'static [Preset] {
    &[
        Preset {
            key: "openrouter",
            display: "OpenRouter",
            base_url: "https://openrouter.ai/api/v1",
            model: "deepseek/deepseek-v4-flash",
            auth_hint: "VULCAN_API_KEY (sk-or-...)",
            disable_catalog: false,
            notes: "Aggregator — most models routable through one endpoint.",
        },
        Preset {
            key: "openai",
            display: "OpenAI",
            base_url: "https://api.openai.com/v1",
            model: "gpt-5",
            auth_hint: "OPENAI_API_KEY or VULCAN_API_KEY",
            disable_catalog: false,
            notes: "First-party endpoint; supports tools, structured output, vision.",
        },
        Preset {
            key: "anthropic",
            display: "Anthropic",
            base_url: "https://api.anthropic.com/v1",
            model: "claude-opus-4-7",
            auth_hint: "ANTHROPIC_API_KEY or VULCAN_API_KEY",
            disable_catalog: false,
            notes: "Claude family. Native API; OpenAI-compat path may need explicit headers.",
        },
        Preset {
            key: "deepseek",
            display: "DeepSeek (direct)",
            base_url: "https://api.deepseek.com/v1",
            model: "deepseek-chat",
            auth_hint: "DEEPSEEK_API_KEY or VULCAN_API_KEY",
            disable_catalog: false,
            notes: "Direct route — bypasses OpenRouter when you want lower latency / no markup.",
        },
        Preset {
            key: "groq",
            display: "Groq",
            base_url: "https://api.groq.com/openai/v1",
            model: "llama-3.3-70b-versatile",
            auth_hint: "GROQ_API_KEY or VULCAN_API_KEY",
            disable_catalog: false,
            notes: "Hosted LPU inference; Llama / Mixtral / Qwen at very high tok/s.",
        },
        Preset {
            key: "together",
            display: "Together AI",
            base_url: "https://api.together.xyz/v1",
            model: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            auth_hint: "TOGETHER_API_KEY or VULCAN_API_KEY",
            disable_catalog: false,
            notes: "Wide open-weights selection.",
        },
        Preset {
            key: "fireworks",
            display: "Fireworks AI",
            base_url: "https://api.fireworks.ai/inference/v1",
            model: "accounts/fireworks/models/qwen2p5-coder-32b-instruct",
            auth_hint: "FIREWORKS_API_KEY or VULCAN_API_KEY",
            disable_catalog: false,
            notes: "Fast hosted inference; strong coder models.",
        },
        Preset {
            key: "ollama",
            display: "Ollama (local)",
            base_url: "http://localhost:11434/v1",
            model: "qwen2.5-coder:latest",
            auth_hint: "no auth required (set api_key = \"ollama\" if your build expects it)",
            disable_catalog: true,
            notes: "Local self-hosted; catalog disabled because it doesn't publish an OpenAI-shape `/models`.",
        },
    ]
}

pub fn lookup_preset(key: &str) -> Option<&'static Preset> {
    presets().iter().find(|p| p.key.eq_ignore_ascii_case(key))
}

pub async fn run(cmd: Option<ProviderCommand>, dir: PathBuf) -> Result<()> {
    match cmd {
        None => interactive_menu(&dir),
        Some(ProviderCommand::List) => list(&dir),
        Some(ProviderCommand::Presets) => {
            print_presets();
            Ok(())
        }
        Some(ProviderCommand::Add(args)) => add(args, &dir),
        Some(ProviderCommand::Remove(args)) => remove(args, &dir),
        Some(ProviderCommand::Use(args)) => use_profile(args, &dir),
    }
}

/// Interactive menu when `vulcan provider` is called without a subcommand.
fn interactive_menu(dir: &Path) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!(
            "vulcan provider (interactive) requires a terminal. Use `vulcan provider list/add/remove/use <args>` for scripting."
        );
    }

    let theme = dialoguer::theme::ColorfulTheme::default();
    let options = &[
        "List providers",
        "Add provider",
        "Remove provider",
        "Use provider",
    ];
    println!();
    let pick = dialoguer::FuzzySelect::with_theme(&theme)
        .with_prompt("Provider action")
        .items(options)
        .default(0)
        .interact()
        .context("cancelled")?;

    match pick {
        0 => list(dir),
        1 => interactive_add(dir),
        2 => interactive_remove(dir),
        3 => interactive_use(dir),
        _ => unreachable!(),
    }
}

/// Interactive add: pick a preset, enter a name, confirm.
fn interactive_add(dir: &Path) -> Result<()> {
    let theme = dialoguer::theme::ColorfulTheme::default();
    let ps = presets();
    let labels: Vec<String> = ps
        .iter()
        .map(|p| format!("{} ({})", p.display, p.key))
        .collect();

    println!();
    let pick = dialoguer::FuzzySelect::with_theme(&theme)
        .with_prompt("Pick a preset")
        .items(&labels)
        .default(0)
        .interact()
        .context("picker cancelled")?;

    let preset = &ps[pick];
    println!();
    println!("  {} ({})", preset.display, preset.key);
    println!("  base_url: {}", preset.base_url);
    println!("  model:    {}", preset.model);

    let name: String = dialoguer::Input::with_theme(&theme)
        .with_prompt("Profile name")
        .default(preset.key.to_string())
        .interact_text()?;

    let confirmed = dialoguer::Confirm::with_theme(&theme)
        .with_prompt(format!("Add [{}] from {} preset?", name, preset.key))
        .default(true)
        .interact()?;

    if !confirmed {
        println!("Aborted.");
        return Ok(());
    }

    add(
        AddArgs {
            name,
            preset: Some(preset.key.to_string()),
            base_url: None,
            model: None,
            api_key: None,
            max_context: None,
            disable_catalog: false,
            force: false,
        },
        dir,
    )?;
    println!(
        "{}",
        "Done. Switch with `/provider <name>` in the TUI or `vulcan provider use <name>`.".dimmed()
    );
    Ok(())
}

/// Interactive remove: pick from existing providers.
fn interactive_remove(dir: &Path) -> Result<()> {
    let cfg = crate::config::Config::load_from_dir(dir).unwrap_or_default();
    let names: Vec<&String> = cfg.providers.keys().collect();
    if names.is_empty() {
        bail!("No named provider profiles to remove.");
    }

    let theme = dialoguer::theme::ColorfulTheme::default();
    println!();
    let pick = dialoguer::FuzzySelect::with_theme(&theme)
        .with_prompt("Pick a provider to remove")
        .items(&names)
        .default(0)
        .interact()
        .context("cancelled")?;

    let name = names[pick].clone();
    let confirmed = dialoguer::Confirm::with_theme(&theme)
        .with_prompt(format!("Permanently remove [{}]?", name))
        .default(false)
        .interact()?;

    if !confirmed {
        println!("Aborted.");
        return Ok(());
    }

    remove(RemoveArgs { name: name.clone() }, dir)
}

/// Interactive use: pick from existing providers.
fn interactive_use(dir: &Path) -> Result<()> {
    let cfg = crate::config::Config::load_from_dir(dir).unwrap_or_default();
    let names: Vec<&String> = cfg.providers.keys().collect();
    if names.is_empty() {
        bail!("No named provider profiles to switch to. Add one first.");
    }

    let theme = dialoguer::theme::ColorfulTheme::default();
    println!();
    let pick = dialoguer::FuzzySelect::with_theme(&theme)
        .with_prompt("Pick a provider to activate")
        .items(&names)
        .default(0)
        .interact()
        .context("cancelled")?;

    let name = names[pick].clone();
    use_profile(
        UseArgs {
            name: Some(name.clone()),
            clear: false,
        },
        dir,
    )
}

fn list(dir: &Path) -> Result<()> {
    let cfg = crate::config::Config::load_from_dir(dir).unwrap_or_default();
    println!("{}", "Provider profiles:".bold());
    println!(
        "  {} {}",
        "(default)".dimmed(),
        format!("{} · {}", cfg.provider.base_url, cfg.provider.model)
    );
    let mut names: Vec<&String> = cfg.providers.keys().collect();
    names.sort();
    for name in &names {
        let p = &cfg.providers[*name];
        let active = match &cfg.active_profile {
            Some(a) if a == *name => " ●".green().to_string(),
            _ => String::new(),
        };
        println!("  {}{}", name.bold(), active,);
        println!("    {} {}", "base_url:".dimmed(), p.base_url);
        println!("    {} {}", "model:".dimmed(), p.model.cyan());
    }
    if cfg.providers.is_empty() {
        println!(
            "    {}",
            "(no named profiles configured — add one with `vulcan provider add`)".dimmed()
        );
    }
    Ok(())
}

fn print_presets() {
    println!("{}", "Curated provider presets:".bold());
    for p in presets() {
        println!();
        println!("  {} {}", p.display.bold(), format!("({})", p.key).dimmed());
        println!("    {}: {}", "base_url".dimmed(), p.base_url);
        println!("    {}: {}", "default_model".dimmed(), p.model.cyan());
        println!("    {}: {}", "auth".dimmed(), p.auth_hint.yellow());
        if p.disable_catalog {
            println!(
                "    {}: {}",
                "catalog".dimmed(),
                "disabled (self-hosted endpoint)".red()
            );
        }
        if !p.notes.is_empty() {
            println!("    {}: {}", "notes".dimmed(), p.notes);
        }
    }
    println!();
    println!(
        "{}",
        "Add via:  vulcan provider add <name> --preset <key>".dimmed()
    );
}

pub fn add(args: AddArgs, dir: &Path) -> Result<()> {
    if args.name.eq_ignore_ascii_case("default") {
        bail!("'default' is reserved for the legacy [provider] block in config.toml.");
    }

    let mut base_url = args.base_url.clone();
    let mut model = args.model.clone();
    let mut disable_catalog = args.disable_catalog;
    if let Some(preset_key) = &args.preset {
        let preset = lookup_preset(preset_key).ok_or_else(|| {
            anyhow!(
                "unknown preset '{preset_key}'. Run `vulcan provider presets` to see the catalog."
            )
        })?;
        base_url.get_or_insert_with(|| preset.base_url.to_string());
        model.get_or_insert_with(|| preset.model.to_string());
        if !disable_catalog {
            disable_catalog = preset.disable_catalog;
        }
    }
    let base_url = base_url
        .ok_or_else(|| anyhow!("--base-url required (or use --preset <key> to inherit one)"))?;
    let model =
        model.ok_or_else(|| anyhow!("--model required (or use --preset <key> to inherit one)"))?;

    let providers_path = dir.join("providers.toml");
    let mut doc = read_or_init_doc(&providers_path)?;

    if doc.contains_key(&args.name) && !args.force {
        bail!(
            "Provider '{}' already exists in {}. Re-run with --force to overwrite.",
            args.name,
            providers_path.display()
        );
    }

    let mut entry = Table::new();
    entry.set_implicit(false);
    entry.insert("type", value("openai-compat"));
    entry.insert("base_url", value(base_url));
    entry.insert("model", value(model));
    if let Some(key) = args.api_key {
        entry.insert("api_key", value(key));
    }
    if let Some(max_ctx) = args.max_context {
        entry.insert("max_context", value(max_ctx as i64));
    }
    if disable_catalog {
        entry.insert("disable_catalog", value(true));
    }
    doc.insert(&args.name, Item::Table(entry));

    write_doc(&providers_path, &doc)?;
    println!(
        "{} Wrote [{}] to {}",
        "✓".green(),
        args.name,
        providers_path.display()
    );
    println!(
        "{}",
        "Use `/provider <name>` in the TUI to switch.".dimmed()
    );
    Ok(())
}

fn remove(args: RemoveArgs, dir: &Path) -> Result<()> {
    if args.name.eq_ignore_ascii_case("default") {
        bail!(
            "'default' refers to the legacy [provider] block in config.toml — edit that file directly."
        );
    }
    let providers_path = dir.join("providers.toml");
    if !providers_path.exists() {
        bail!(
            "no providers.toml at {} — nothing to remove",
            providers_path.display()
        );
    }
    let mut doc = read_or_init_doc(&providers_path)?;
    if doc.remove(&args.name).is_none() {
        bail!(
            "no provider profile named '{}' in {}",
            args.name,
            providers_path.display()
        );
    }
    write_doc(&providers_path, &doc)?;
    println!(
        "{} Removed [{}] from {}",
        "✓".green(),
        args.name,
        providers_path.display()
    );
    Ok(())
}

fn use_profile(args: UseArgs, dir: &Path) -> Result<()> {
    let config_path = dir.join("config.toml");
    let providers_path = dir.join("providers.toml");
    if args.clear && args.name.is_some() {
        bail!("--clear conflicts with positional name");
    }
    if !args.clear && args.name.is_none() {
        bail!("supply a profile name or pass --clear");
    }

    let mut config_doc = read_or_init_doc(&config_path)?;

    if args.clear {
        let removed = config_doc.remove("active_profile").is_some();
        write_doc(&config_path, &config_doc)?;
        if removed {
            println!(
                "Cleared active_profile from {} — legacy [provider] is now active.",
                config_path.display()
            );
        } else {
            println!("No active_profile set; nothing to clear.");
        }
        return Ok(());
    }

    let name = args.name.expect("name required when --clear absent");
    if name.eq_ignore_ascii_case("default") {
        bail!(
            "'default' refers to the legacy [provider] block — pass `--clear` instead to fall back to it."
        );
    }
    // Validate the profile exists in providers.toml before writing.
    let providers_doc = read_or_init_doc(&providers_path)?;
    if providers_doc.get(&name).is_none() {
        bail!(
            "no provider profile named '{}' in {} — run `vulcan provider list` to see available profiles",
            name,
            providers_path.display()
        );
    }

    config_doc["active_profile"] = value(name.clone());
    write_doc(&config_path, &config_doc)?;
    println!(
        "Set active_profile = \"{}\" in {} — TUI + gateway will boot against [providers.{}]",
        name,
        config_path.display(),
        name
    );
    Ok(())
}

fn read_or_init_doc(path: &Path) -> Result<DocumentMut> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create dir {}", parent.display()))?;
    }
    if !path.exists() {
        return Ok(DocumentMut::new());
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    raw.parse::<DocumentMut>()
        .with_context(|| format!("Failed to parse {}", path.display()))
}

fn write_doc(path: &Path, doc: &DocumentMut) -> Result<()> {
    std::fs::write(path, doc.to_string())
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn presets_catalog_has_expected_minimum() {
        let keys: Vec<_> = presets().iter().map(|p| p.key).collect();
        for must in [
            "openrouter",
            "openai",
            "anthropic",
            "ollama",
            "groq",
            "deepseek",
        ] {
            assert!(keys.contains(&must), "preset {must} missing from catalog");
        }
    }

    // ── YYC-240 (YYC-238 PR-2): vulcan provider use/clear ────────

    #[test]
    fn use_profile_writes_active_profile_when_target_exists() {
        let dir = tempdir().unwrap();
        // Seed providers.toml with one named profile so `use`
        // can find it.
        add(
            AddArgs {
                name: "fast".into(),
                preset: Some("openai".into()),
                base_url: None,
                model: None,
                api_key: None,
                max_context: None,
                disable_catalog: false,
                force: false,
            },
            dir.path(),
        )
        .unwrap();

        use_profile(
            UseArgs {
                name: Some("fast".into()),
                clear: false,
            },
            dir.path(),
        )
        .unwrap();

        let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(raw.contains("active_profile = \"fast\""));
        // Round-trip via Config::load_from_dir picks it up.
        let cfg = crate::config::Config::load_from_dir(dir.path()).unwrap();
        assert_eq!(cfg.active_profile.as_deref(), Some("fast"));
    }

    #[test]
    fn use_profile_rejects_unknown_target_before_writing() {
        let dir = tempdir().unwrap();
        let err = use_profile(
            UseArgs {
                name: Some("ghost".into()),
                clear: false,
            },
            dir.path(),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("no provider profile named 'ghost'")
        );
        // No config.toml written on failure.
        assert!(!dir.path().join("config.toml").exists());
    }

    #[test]
    fn use_profile_clear_removes_active_profile() {
        let dir = tempdir().unwrap();
        add(
            AddArgs {
                name: "fast".into(),
                preset: Some("openai".into()),
                base_url: None,
                model: None,
                api_key: None,
                max_context: None,
                disable_catalog: false,
                force: false,
            },
            dir.path(),
        )
        .unwrap();
        use_profile(
            UseArgs {
                name: Some("fast".into()),
                clear: false,
            },
            dir.path(),
        )
        .unwrap();
        use_profile(
            UseArgs {
                name: None,
                clear: true,
            },
            dir.path(),
        )
        .unwrap();
        let cfg = crate::config::Config::load_from_dir(dir.path()).unwrap();
        assert!(cfg.active_profile.is_none());
    }

    #[test]
    fn use_profile_rejects_conflicting_args() {
        let dir = tempdir().unwrap();
        let err = use_profile(
            UseArgs {
                name: Some("any".into()),
                clear: true,
            },
            dir.path(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("conflicts"));
    }

    #[test]
    fn add_writes_to_providers_toml_only() {
        let dir = tempdir().unwrap();

        add(
            AddArgs {
                name: "local".into(),
                preset: Some("ollama".into()),
                base_url: None,
                model: None,
                api_key: None,
                max_context: None,
                disable_catalog: false,
                force: false,
            },
            dir.path(),
        )
        .unwrap();

        let providers_raw = std::fs::read_to_string(dir.path().join("providers.toml")).unwrap();
        assert!(providers_raw.contains("[local]"));
        assert!(providers_raw.contains("base_url = \"http://localhost:11434/v1\""));
        assert!(providers_raw.contains("disable_catalog = true"));

        // Main config.toml is never touched.
        assert!(!dir.path().join("config.toml").exists());

        // Round-trip via Config::load_from_dir.
        let cfg = crate::config::Config::load_from_dir(dir.path()).unwrap();
        assert_eq!(cfg.providers["local"].model, "qwen2.5-coder:latest");

        // Remove path.
        remove(
            RemoveArgs {
                name: "local".into(),
            },
            dir.path(),
        )
        .unwrap();
        let providers_raw = std::fs::read_to_string(dir.path().join("providers.toml")).unwrap();
        assert!(!providers_raw.contains("[local]"));
    }

    #[test]
    fn add_overrides_preset_with_explicit_model() {
        let dir = tempdir().unwrap();
        add(
            AddArgs {
                name: "or".into(),
                preset: Some("openrouter".into()),
                base_url: None,
                model: Some("anthropic/claude-opus-4-7".into()),
                api_key: None,
                max_context: None,
                disable_catalog: false,
                force: false,
            },
            dir.path(),
        )
        .unwrap();
        let cfg = crate::config::Config::load_from_dir(dir.path()).unwrap();
        assert_eq!(cfg.providers["or"].model, "anthropic/claude-opus-4-7");
        assert_eq!(cfg.providers["or"].base_url, "https://openrouter.ai/api/v1");
    }

    #[test]
    fn add_rejects_default_name() {
        let dir = tempdir().unwrap();
        let err = add(
            AddArgs {
                name: "default".into(),
                preset: Some("openai".into()),
                base_url: None,
                model: None,
                api_key: None,
                max_context: None,
                disable_catalog: false,
                force: false,
            },
            dir.path(),
        )
        .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("reserved"));
    }

    #[test]
    fn add_refuses_overwrite_without_force() {
        let dir = tempdir().unwrap();
        add(
            AddArgs {
                name: "x".into(),
                preset: Some("openrouter".into()),
                base_url: None,
                model: None,
                api_key: None,
                max_context: None,
                disable_catalog: false,
                force: false,
            },
            dir.path(),
        )
        .unwrap();
        let err = add(
            AddArgs {
                name: "x".into(),
                preset: Some("openai".into()),
                base_url: None,
                model: None,
                api_key: None,
                max_context: None,
                disable_catalog: false,
                force: false,
            },
            dir.path(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn remove_unknown_profile_errors() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("providers.toml"), "[other]\n").unwrap();
        let err = remove(
            RemoveArgs {
                name: "nope".into(),
            },
            dir.path(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("nope"));
    }
}
