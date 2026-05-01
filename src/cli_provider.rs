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
    /// Write a new `[<name>]` block in providers.toml. No name opens the guided flow.
    Add(AddArgs),
    /// Delete a named `[<name>]` block from providers.toml. Use `--list` for a picker.
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
    pub name: Option<String>,
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

impl AddArgs {
    fn is_bare_interactive(&self) -> bool {
        self.preset.is_none()
            && self.base_url.is_none()
            && self.model.is_none()
            && self.api_key.is_none()
            && self.max_context.is_none()
            && !self.disable_catalog
            && !self.force
    }
}

#[derive(Args, Debug)]
pub struct RemoveArgs {
    /// Profile name to delete. `default` is reserved for the legacy
    /// `[provider]` block in config.toml and rejected here.
    pub name: Option<String>,
    /// Open a picker to select the profile to delete.
    #[arg(long)]
    pub list: bool,
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
        None => interactive_menu(&dir).await,
        Some(ProviderCommand::List) => list(&dir),
        Some(ProviderCommand::Presets) => {
            print_presets();
            Ok(())
        }
        Some(ProviderCommand::Add(args)) => {
            if args.name.is_none() {
                if args.is_bare_interactive() {
                    interactive_add(&dir).await
                } else {
                    bail!(
                        "profile name required when passing provider add options. Omit options for interactive setup."
                    );
                }
            } else {
                add(args, &dir)
            }
        }
        Some(ProviderCommand::Remove(args)) => remove(args, &dir),
        Some(ProviderCommand::Use(args)) => use_profile(args, &dir),
    }
}

/// Interactive menu when `vulcan provider` is called without a subcommand.
async fn interactive_menu(dir: &Path) -> Result<()> {
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
        1 => interactive_add(dir).await,
        2 => interactive_remove(dir),
        3 => interactive_use(dir),
        _ => unreachable!(),
    }
}

/// Interactive add: reuse the auth preset/name/key/model flow.
async fn interactive_add(dir: &Path) -> Result<()> {
    crate::cli_auth::run(
        crate::cli_auth::AuthArgs {
            preset: None,
            custom: false,
        },
        dir.to_path_buf(),
    )
    .await
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

    remove(
        RemoveArgs {
            name: Some(name.clone()),
            list: false,
        },
        dir,
    )
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
    print_provider_table_header();
    print_provider_row("(default)", &cfg.provider, cfg.active_profile.is_none());
    let mut names: Vec<&String> = cfg.providers.keys().collect();
    names.sort();
    for name in &names {
        let p = &cfg.providers[*name];
        let active = cfg.active_profile.as_ref().is_some_and(|a| a == *name);
        print_provider_row(name, p, active);
    }
    if cfg.providers.is_empty() {
        println!(
            "    {}",
            "(no named profiles configured — add one with `vulcan provider add`)".dimmed()
        );
    }
    let extension_providers = collect_extension_provider_names();
    if !extension_providers.is_empty() {
        println!();
        println!("{}", "Extension providers:".bold());
        for name in extension_providers {
            println!("  {}", name.bold());
            println!(
                "    {} set a profile with {} to select it",
                "usage:".dimmed(),
                format!("type = \"{name}\"").cyan()
            );
        }
    }
    Ok(())
}

fn print_provider_table_header() {
    println!(
        "  {:<18} {:<12} {:<38} {:<30} {:<10}",
        "name".dimmed(),
        "preset".dimmed(),
        "base URL".dimmed(),
        "model".dimmed(),
        "last used".dimmed()
    );
}

fn print_provider_row(name: &str, p: &crate::config::ProviderConfig, active: bool) {
    let marker = if active {
        "●".green().to_string()
    } else {
        " ".into()
    };
    println!(
        "{} {:<18} {:<12} {:<38} {:<30} {:<10}",
        marker,
        name.bold(),
        provider_preset_label(p).cyan(),
        p.base_url,
        p.model,
        "never".dimmed()
    );
}

fn provider_preset_label(p: &crate::config::ProviderConfig) -> &'static str {
    presets()
        .iter()
        .find(|preset| preset.base_url == p.base_url)
        .map(|preset| preset.key)
        .unwrap_or("custom")
}

fn collect_extension_provider_names() -> Vec<String> {
    let registry = crate::extensions::ExtensionRegistry::new();
    crate::extensions::api::wire_inventory_into_registry(&registry);
    let catalog = crate::provider::factory::ExtensionProviderCatalog::new();
    let ctx = crate::extensions::api::SessionExtensionCtx {
        cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        session_id: "__provider_list__".into(),
        memory: std::sync::Arc::new(crate::memory::SessionStore::in_memory()),
        frontend_capabilities: crate::extensions::FrontendCapability::full_set(),
        state: crate::extensions::ExtensionStateContext::in_memory_for_tests(
            "__provider_list__",
            "__pending__",
        ),
    };
    registry.wire_daemon_extension_providers(ctx, &catalog);
    catalog.names()
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
    let name = args
        .name
        .as_deref()
        .ok_or_else(|| anyhow!("profile name required for non-interactive provider add"))?;
    if name.eq_ignore_ascii_case("default") {
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

    if doc.contains_key(name) && !args.force {
        bail!(
            "Provider '{}' already exists in {}. Re-run with --force to overwrite.",
            name,
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
    doc.insert(name, Item::Table(entry));

    write_doc(&providers_path, &doc)?;
    println!(
        "{} Wrote [{}] to {}",
        "✓".green(),
        name,
        providers_path.display()
    );
    println!(
        "{}",
        "Use `/provider <name>` in the TUI to switch.".dimmed()
    );
    Ok(())
}

fn remove(args: RemoveArgs, dir: &Path) -> Result<()> {
    if args.list {
        if args.name.is_some() {
            bail!("--list conflicts with positional name");
        }
        if !std::io::stdin().is_terminal() {
            bail!(
                "vulcan provider remove --list requires a terminal. Use `vulcan provider remove <name>` for scripting."
            );
        }
        return interactive_remove(dir);
    }

    let name = args
        .name
        .as_deref()
        .ok_or_else(|| anyhow!("supply a profile name or pass --list"))?;
    if name.eq_ignore_ascii_case("default") {
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
    if doc.remove(name).is_none() {
        bail!(
            "no provider profile named '{}' in {}",
            name,
            providers_path.display()
        );
    }
    write_doc(&providers_path, &doc)?;
    println!(
        "{} Removed [{}] from {}",
        "✓".green(),
        name,
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
    use clap::Parser;
    use tempfile::tempdir;

    #[test]
    fn provider_add_without_args_parses_for_interactive_flow() {
        let cli = crate::cli::Cli::try_parse_from(["vulcan", "provider", "add"]).unwrap();
        let crate::cli::Command::Provider {
            cmd: Some(ProviderCommand::Add(args)),
        } = cli.command.unwrap()
        else {
            panic!("expected provider add command");
        };
        assert!(args.name.is_none());
        assert!(args.preset.is_none());
    }

    #[test]
    fn provider_add_explicit_args_still_parse() {
        let cli = crate::cli::Cli::try_parse_from([
            "vulcan", "provider", "add", "local", "--preset", "ollama", "--model", "qwen",
        ])
        .unwrap();
        let crate::cli::Command::Provider {
            cmd: Some(ProviderCommand::Add(args)),
        } = cli.command.unwrap()
        else {
            panic!("expected provider add command");
        };
        assert_eq!(args.name.as_deref(), Some("local"));
        assert_eq!(args.preset.as_deref(), Some("ollama"));
        assert_eq!(args.model.as_deref(), Some("qwen"));
    }

    #[test]
    fn provider_add_options_without_name_are_not_bare_interactive() {
        let cli =
            crate::cli::Cli::try_parse_from(["vulcan", "provider", "add", "--preset", "openai"])
                .unwrap();
        let crate::cli::Command::Provider {
            cmd: Some(ProviderCommand::Add(args)),
        } = cli.command.unwrap()
        else {
            panic!("expected provider add command");
        };
        assert!(args.name.is_none());
        assert!(!args.is_bare_interactive());
    }

    #[test]
    fn provider_remove_list_parses_for_picker_flow() {
        let cli =
            crate::cli::Cli::try_parse_from(["vulcan", "provider", "remove", "--list"]).unwrap();
        let crate::cli::Command::Provider {
            cmd: Some(ProviderCommand::Remove(args)),
        } = cli.command.unwrap()
        else {
            panic!("expected provider remove command");
        };
        assert!(args.name.is_none());
        assert!(args.list);
    }

    #[test]
    fn provider_remove_explicit_name_still_parses() {
        let cli =
            crate::cli::Cli::try_parse_from(["vulcan", "provider", "remove", "local"]).unwrap();
        let crate::cli::Command::Provider {
            cmd: Some(ProviderCommand::Remove(args)),
        } = cli.command.unwrap()
        else {
            panic!("expected provider remove command");
        };
        assert_eq!(args.name.as_deref(), Some("local"));
        assert!(!args.list);
    }

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

    #[test]
    fn provider_preset_label_infers_known_base_url() {
        let mut provider = crate::config::ProviderConfig {
            base_url: "https://openrouter.ai/api/v1".into(),
            ..Default::default()
        };
        assert_eq!(provider_preset_label(&provider), "openrouter");

        provider.base_url = "https://example.com/v1".into();
        assert_eq!(provider_preset_label(&provider), "custom");
    }

    // ── YYC-240 (YYC-238 PR-2): vulcan provider use/clear ────────

    #[test]
    fn use_profile_writes_active_profile_when_target_exists() {
        let dir = tempdir().unwrap();
        // Seed providers.toml with one named profile so `use`
        // can find it.
        add(
            AddArgs {
                name: Some("fast".into()),
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
                name: Some("fast".into()),
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
                name: Some("local".into()),
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
                name: Some("local".into()),
                list: false,
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
                name: Some("or".into()),
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
                name: Some("default".into()),
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
                name: Some("x".into()),
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
                name: Some("x".into()),
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
                name: Some("nope".into()),
                list: false,
            },
            dir.path(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("nope"));
    }

    #[test]
    fn remove_requires_name_or_list() {
        let dir = tempdir().unwrap();
        let err = remove(
            RemoveArgs {
                name: None,
                list: false,
            },
            dir.path(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("supply a profile name"));
    }

    #[test]
    fn remove_list_conflicts_with_name() {
        let dir = tempdir().unwrap();
        let err = remove(
            RemoveArgs {
                name: Some("local".into()),
                list: true,
            },
            dir.path(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("conflicts"));
    }
}
