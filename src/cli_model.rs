//! YYC-241: `vulcan model list/show/use` for the active provider.
//! YYC-288: interactive picker from catalog + styled output.

use anyhow::{Context, Result, anyhow, bail};
use owo_colors::OwoColorize;
use std::io::IsTerminal;
use std::time::Duration;
use toml_edit::{DocumentMut, value};

use crate::cli::ModelSubcommand;
use crate::config::{Config, ProviderConfig, vulcan_home};

pub async fn run(cmd: Option<ModelSubcommand>) -> Result<()> {
    let dir = vulcan_home();
    let config = Config::load_from_dir(&dir).unwrap_or_default();
    match cmd {
        None => interactive_pick(&config).await,
        Some(ModelSubcommand::List) => list(&config).await,
        Some(ModelSubcommand::Show) => show(&config),
        Some(ModelSubcommand::Use {
            id: Some(id),
            force,
        }) => use_model(&dir, &config, &id, force).await,
        Some(ModelSubcommand::Use { id: None, force }) => {
            // Interactive pick then use
            let id = interactive_pick_id(&config).await?;
            use_model(&dir, &config, &id, force).await
        }
    }
}

/// No subcommand → fuzzy picker of models from catalog.
/// Selecting one prompts to use it.
async fn interactive_pick(config: &Config) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!(
            "vulcan model (interactive) requires a terminal. Use `vulcan model list` to browse, or `vulcan model use <id>` to set."
        );
    }

    let id = interactive_pick_id(config).await?;
    let dir = vulcan_home();
    let force = false;
    use_model(&dir, config, &id, force).await
}

/// Fuzzy pick a model from the catalog. Returns the chosen model id.
async fn interactive_pick_id(config: &Config) -> Result<String> {
    let provider = config.active_provider_config();
    let api_key = config.api_key().unwrap_or_else(String::new);
    let models = fetch_catalog(provider, &api_key)
        .await
        .with_context(|| format!("failed to fetch /models from {}", provider.base_url))?;

    if models.is_empty() {
        bail!("No models available in the catalog.");
    }

    let theme = dialoguer::theme::ColorfulTheme::default();
    let labels: Vec<String> = models
        .iter()
        .map(|m| {
            let ctx = if m.context_length > 0 {
                format!(" {}", m.context_length.to_string().dimmed())
            } else {
                String::new()
            };
            format!("{}{}", m.id, ctx)
        })
        .collect();

    println!();
    let pick = dialoguer::FuzzySelect::with_theme(&theme)
        .with_prompt("Pick a model")
        .items(&labels)
        .default(0)
        .interact()
        .context("picker cancelled")?;

    Ok(models[pick].id.clone())
}

fn show(config: &Config) -> Result<()> {
    let provider = config.active_provider_config();
    let label = config
        .active_profile
        .as_deref()
        .unwrap_or("[provider] (legacy)");
    println!("{}", "Active provider:".bold());
    println!("  {} {label}", "profile:".dimmed());
    println!("  {}  {}", "base_url:".dimmed(), provider.base_url);
    println!("  {}      {}", "model:".dimmed(), provider.model.green());
    Ok(())
}

async fn list(config: &Config) -> Result<()> {
    let provider = config.active_provider_config();
    let api_key = config.api_key().unwrap_or_else(String::new);
    let models = fetch_catalog(provider, &api_key).await.with_context(|| {
        format!(
            "fetch /models from {} (set api_key or VULCAN_API_KEY first)",
            provider.base_url
        )
    })?;
    if models.is_empty() {
        println!("(catalog empty)");
        return Ok(());
    }
    // Styled header
    println!(
        "{} {} {}",
        "id".bold().white().on_blue(),
        "context".bold().white().on_blue(),
        "display".bold().white().on_blue(),
    );
    for m in models {
        let ctx = if m.context_length > 0 {
            m.context_length.to_string().cyan().to_string()
        } else {
            "-".dimmed().to_string()
        };
        println!("{:<40} {:<10} {}", m.id, ctx, m.display_name.dimmed());
    }
    Ok(())
}

async fn use_model(dir: &std::path::Path, config: &Config, id: &str, force: bool) -> Result<()> {
    let provider = config.active_provider_config();

    if !force && !provider.disable_catalog {
        let api_key = config.api_key().unwrap_or_default();
        match fetch_catalog(provider, &api_key).await {
            Ok(models) if models.iter().any(|m| m.id == id) => {}
            Ok(_) => bail!(
                "model `{id}` not in {}'s catalog. Re-run with --force to set anyway.",
                provider.base_url
            ),
            Err(e) => {
                tracing::warn!("catalog fetch failed: {e}; proceeding with --force semantics");
            }
        }
    }

    // Determine which file + section to write into.
    let active_profile = config.active_profile.as_deref();
    match active_profile {
        Some(name) => write_named_provider_model(dir, name, id)?,
        None => write_legacy_provider_model(dir, id)?,
    }
    let target = active_profile
        .map(|n| format!("[providers.{n}]"))
        .unwrap_or_else(|| "[provider]".to_string());
    println!("{} set model = \"{id}\" on {target}", "✓".green());
    Ok(())
}

fn write_named_provider_model(dir: &std::path::Path, profile: &str, id: &str) -> Result<()> {
    let path = dir.join("providers.toml");
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut doc: DocumentMut = raw
        .parse()
        .with_context(|| format!("parse {}", path.display()))?;
    let table = doc
        .get_mut(profile)
        .ok_or_else(|| anyhow!("[{profile}] not found in {}", path.display()))?;
    let table = table
        .as_table_mut()
        .ok_or_else(|| anyhow!("[{profile}] is not a table"))?;
    table["model"] = value(id);
    std::fs::write(&path, doc.to_string()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn write_legacy_provider_model(dir: &std::path::Path, id: &str) -> Result<()> {
    let path = dir.join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc: DocumentMut = if raw.is_empty() {
        DocumentMut::new()
    } else {
        raw.parse()
            .with_context(|| format!("parse {}", path.display()))?
    };
    let entry = doc
        .entry("provider")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let table = entry
        .as_table_mut()
        .ok_or_else(|| anyhow!("[provider] is not a table"))?;
    table["model"] = value(id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, doc.to_string()).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub async fn fetch_catalog(
    provider: &ProviderConfig,
    api_key: &str,
) -> Result<Vec<crate::provider::catalog::ModelInfo>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let ttl = Duration::from_secs(provider.catalog_cache_ttl_hours * 3600);
    let catalog = crate::provider::catalog::for_base_url(client, &provider.base_url, api_key, ttl);
    catalog.list_models().await.map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn use_model_writes_to_named_provider_when_active_profile_set() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "active_profile = \"fast\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("providers.toml"),
            r#"[fast]
type = "openai-compat"
base_url = "https://example.com"
model = "old-model"
disable_catalog = true
"#,
        )
        .unwrap();
        write_named_provider_model(dir.path(), "fast", "new-model").unwrap();
        let raw = std::fs::read_to_string(dir.path().join("providers.toml")).unwrap();
        assert!(raw.contains("model = \"new-model\""));
    }

    #[test]
    fn use_model_writes_to_legacy_provider_when_no_active_profile() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"[provider]
type = "openai-compat"
base_url = "https://example.com"
model = "old"
disable_catalog = true
"#,
        )
        .unwrap();
        write_legacy_provider_model(dir.path(), "new").unwrap();
        let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(raw.contains("model = \"new\""));
        let cfg = Config::load_from_dir(dir.path()).unwrap();
        assert_eq!(cfg.provider.model, "new");
    }

    #[test]
    fn use_model_errors_when_named_profile_missing() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("providers.toml"), "").unwrap();
        let err = write_named_provider_model(dir.path(), "ghost", "x").unwrap_err();
        assert!(err.to_string().contains("[ghost]"));
    }
}
