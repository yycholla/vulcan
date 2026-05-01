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
    let theme = dialoguer::theme::ColorfulTheme::default();
    let confirmed = dialoguer::Confirm::with_theme(&theme)
        .with_prompt(format!("Use {id} as the active model?"))
        .default(true)
        .interact()
        .context("confirmation cancelled")?;
    if !confirmed {
        println!("{}", "No changes.".dimmed());
        return Ok(());
    }

    let dir = vulcan_home();
    let force = false;
    use_model(&dir, config, &id, force).await
}

/// Fuzzy pick a model from the catalog. Returns the chosen model id.
async fn interactive_pick_id(config: &Config) -> Result<String> {
    if !std::io::stdin().is_terminal() {
        bail!(
            "interactive model selection requires a terminal. Use `vulcan model list` to browse, or `vulcan model use <id>` to set."
        );
    }

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
        .map(|m| format_model_picker_label(m))
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
    let label = active_provider_label(config);
    println!("{}", render_active_model_line(&label, provider));
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
    let label = active_provider_label(config);
    print!("{}", render_model_list(&label, &models));
    Ok(())
}

fn active_provider_label(config: &Config) -> String {
    config
        .active_profile
        .as_deref()
        .filter(|name| config.providers.contains_key(*name))
        .unwrap_or("legacy")
        .to_string()
}

fn format_model_picker_label(model: &crate::provider::catalog::ModelInfo) -> String {
    if model.context_length > 0 {
        format!(
            "{}  · ctx {}",
            model.id,
            format_token_count(model.context_length).dimmed()
        )
    } else {
        format!("{}  · ctx {}", model.id, "unknown".dimmed())
    }
}

fn render_active_model_line(label: &str, provider: &ProviderConfig) -> String {
    let active = format!("{label} · {}", provider.model);
    format!(
        "{} {} {} {}\n",
        "Active model:".bold(),
        active.green(),
        "· context".dimmed(),
        format_token_count(provider.max_context).cyan()
    )
}

fn render_model_list(label: &str, models: &[crate::provider::catalog::ModelInfo]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<44} {:>12}  {}\n",
        "MODEL ID".bold().white().on_blue(),
        "CONTEXT".bold().white().on_blue(),
        "PROVIDER".bold().white().on_blue()
    ));
    for model in models {
        let context = if model.context_length > 0 {
            format_token_count(model.context_length).cyan().to_string()
        } else {
            "unknown".dimmed().to_string()
        };
        out.push_str(&format!("{:<44} {:>12}  {}\n", model.id, context, label));
    }
    out
}

fn format_token_count(value: usize) -> String {
    let raw = value.to_string();
    let mut out = String::with_capacity(raw.len() + raw.len() / 3);
    let first_group = raw.len() % 3;
    for (idx, ch) in raw.chars().enumerate() {
        if idx > 0 && (idx == first_group || (idx > first_group && (idx - first_group) % 3 == 0)) {
            out.push(',');
        }
        out.push(ch);
    }
    out
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
    use crate::provider::catalog::{ModelFeatures, ModelInfo};
    use tempfile::tempdir;

    fn model(id: &str, display_name: &str, context_length: usize) -> ModelInfo {
        ModelInfo {
            id: id.to_string(),
            display_name: display_name.to_string(),
            context_length,
            pricing: None,
            features: ModelFeatures::default(),
            top_provider: None,
        }
    }

    #[test]
    fn model_list_rows_include_id_context_and_provider() {
        let models = vec![
            model("openai/gpt-5", "GPT-5", 400_000),
            model("openai/o4-mini", "o4 mini", 0),
        ];

        let rendered = render_model_list("openrouter", &models);

        assert!(rendered.contains("MODEL ID"));
        assert!(rendered.contains("CONTEXT"));
        assert!(rendered.contains("PROVIDER"));
        assert!(rendered.contains("openai/gpt-5"));
        assert!(rendered.contains("400,000"));
        assert!(rendered.contains("openrouter"));
        assert!(rendered.contains("unknown"));
    }

    #[test]
    fn active_model_line_includes_provider_model_and_context() {
        let provider = ProviderConfig {
            model: "gpt-5".into(),
            max_context: 400_000,
            ..ProviderConfig::default()
        };

        let rendered = render_active_model_line("fast", &provider);

        assert!(rendered.contains("fast"));
        assert!(rendered.contains("gpt-5"));
        assert!(rendered.contains("400,000"));
    }

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
