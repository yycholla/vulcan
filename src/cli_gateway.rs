//! `vulcan gateway ...` helpers.

use std::path::Path;

use anyhow::{Context, Result, bail};
use toml_edit::{DocumentMut, Item, Table, value};

use crate::config::Config;

/// Bootstrap a `[gateway]` section in `config.toml`.
///
/// The gateway's LLM routing is deliberately not duplicated here: it uses
/// the same active provider resolution as the TUI (`active_profile`, or the
/// legacy `[provider]` block when unset). This command only creates the
/// HTTP/connector settings needed before `vulcan gateway run` can bind.
pub fn init(dir: &Path, config: &Config, force: bool) -> Result<()> {
    let config_path = dir.join("config.toml");
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create dir {}", parent.display()))?;
    }

    let raw = std::fs::read_to_string(&config_path).unwrap_or_default();
    let mut doc: DocumentMut = if raw.trim().is_empty() {
        DocumentMut::new()
    } else {
        raw.parse()
            .with_context(|| format!("Failed to parse {}", config_path.display()))?
    };

    if doc.get("gateway").is_some() && !force {
        bail!(
            "{} already has a [gateway] section; re-run with --force to replace it",
            config_path.display()
        );
    }

    let mut gateway = Table::new();
    gateway.set_implicit(false);
    gateway.insert("bind", value("127.0.0.1:7777"));
    gateway.insert("api_token", value(generate_api_token()));
    gateway.insert("idle_ttl_secs", value(1800));
    gateway.insert("max_concurrent_lanes", value(64));
    gateway.insert("outbound_max_attempts", value(5));
    doc.insert("gateway", Item::Table(gateway));

    crate::config::atomic_write(&config_path, &doc.to_string())?;

    let provider_label = config
        .active_profile
        .as_deref()
        .map(|name| format!("[providers.{name}]"))
        .unwrap_or_else(|| "[provider]".to_string());
    println!("Wrote [gateway] to {}", config_path.display());
    println!("Gateway agents will use the active provider config: {provider_label}");
    Ok(())
}

fn generate_api_token() -> String {
    format!("vulcan-gw-{}", uuid::Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn init_writes_gateway_section_without_touching_active_profile() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"active_profile = "fast"

[provider]
model = "legacy"
"#,
        )
        .unwrap();
        let config = Config {
            active_profile: Some("fast".into()),
            ..Config::default()
        };

        init(dir.path(), &config, false).unwrap();

        let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(raw.contains("active_profile = \"fast\""));
        assert!(raw.contains("[gateway]"));
        assert!(raw.contains("bind = \"127.0.0.1:7777\""));
        assert!(raw.contains("api_token = \"vulcan-gw-"));
        let loaded = Config::load_from_dir(dir.path()).unwrap();
        loaded.gateway.expect("gateway").validate().unwrap();
    }

    #[test]
    fn init_refuses_existing_gateway_without_force() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"[gateway]
api_token = "keep"
"#,
        )
        .unwrap();

        let err = init(dir.path(), &Config::default(), false).unwrap_err();

        assert!(err.to_string().contains("already has a [gateway] section"));
        let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(raw.contains("api_token = \"keep\""));
    }

    #[test]
    fn init_force_replaces_existing_gateway() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"[gateway]
api_token = "old"
bind = "127.0.0.1:9999"
"#,
        )
        .unwrap();

        init(dir.path(), &Config::default(), true).unwrap();

        let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(!raw.contains("api_token = \"old\""));
        assert!(raw.contains("bind = \"127.0.0.1:7777\""));
    }
}
