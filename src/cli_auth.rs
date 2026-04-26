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
use std::time::Duration;

use crate::cli_provider::{AddArgs, Preset, add, lookup_preset, presets};
use crate::provider::catalog::{self, ModelInfo};

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
        return run_custom(&theme, &dir).await;
    }

    if let Some(key) = args.preset.as_deref() {
        let preset = lookup_preset(key)
            .with_context(|| format!("unknown preset '{key}'. See `vulcan provider presets`."))?;
        return run_preset(&theme, &dir, preset).await;
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
        run_custom(&theme, &dir).await
    } else {
        run_preset(&theme, &dir, &presets[pick]).await
    }
}

async fn run_preset(
    theme: &dyn dialoguer::theme::Theme,
    dir: &Path,
    preset: &Preset,
) -> Result<()> {
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
        .validate_with(validate_profile_name)
        .interact_text()?;

    let api_key_opt = prompt_api_key(theme, preset.base_url, preset.auth_hint)?;

    let model = pick_or_input_model(
        theme,
        preset.base_url,
        api_key_opt.as_deref().unwrap_or(""),
        preset.model,
        preset.disable_catalog,
    )
    .await?;

    let force = profile_exists_prompt(theme, dir, &name)?;

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

async fn run_custom(theme: &dyn dialoguer::theme::Theme, dir: &Path) -> Result<()> {
    println!();
    println!("Custom provider — enter the OpenAI-compatible endpoint by hand.");
    println!();

    let name: String = Input::with_theme(theme)
        .with_prompt("Profile name")
        .validate_with(validate_profile_name)
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

    let api_key_opt = prompt_api_key(
        theme,
        &base_url,
        "auth resolves via VULCAN_API_KEY when blank",
    )?;

    let disable_catalog = if is_local_endpoint(&base_url) {
        // Local endpoints (Ollama, llama.cpp, vLLM, etc.) frequently
        // don't ship an OpenAI-shape `/models` route. Default to off
        // so the agent doesn't fail at startup probing it.
        Confirm::with_theme(theme)
            .with_prompt("Local endpoint detected — skip the /models catalog fetch at startup?")
            .default(true)
            .interact()?
    } else {
        Confirm::with_theme(theme)
            .with_prompt("Skip the /models catalog fetch at startup? (rare; mainly for self-hosted endpoints)")
            .default(false)
            .interact()?
    };

    let model = pick_or_input_model(
        theme,
        &base_url,
        api_key_opt.as_deref().unwrap_or(""),
        "",
        disable_catalog,
    )
    .await?;

    let force = profile_exists_prompt(theme, dir, &name)?;

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

fn validate_profile_name(input: &String) -> Result<(), &'static str> {
    if input.eq_ignore_ascii_case("default") {
        Err("'default' is reserved for the legacy [provider] block")
    } else if input.trim().is_empty() {
        Err("name required")
    } else {
        Ok(())
    }
}

fn profile_exists_prompt(
    theme: &dyn dialoguer::theme::Theme,
    dir: &Path,
    name: &str,
) -> Result<bool> {
    if profile_exists(dir, name)? {
        Ok(Confirm::with_theme(theme)
            .with_prompt(format!(
                "Profile '{name}' already exists in providers.toml. Overwrite?"
            ))
            .default(false)
            .interact()?)
    } else {
        Ok(false)
    }
}

/// Prompt for an API key, but skip the prompt entirely when the
/// endpoint looks local — most self-hosted endpoints (Ollama, llama.cpp,
/// vLLM in unauth mode) don't need one. Returns `None` when the user
/// declines to set a key (or local + opted out), `Some(s)` otherwise.
fn prompt_api_key(
    theme: &dyn dialoguer::theme::Theme,
    base_url: &str,
    auth_hint: &str,
) -> Result<Option<String>> {
    if is_local_endpoint(base_url) {
        let want = Confirm::with_theme(theme)
            .with_prompt(
                "Local endpoint detected — set an API key? (most self-hosted servers don't need one)",
            )
            .default(false)
            .interact()?;
        if !want {
            return Ok(None);
        }
        let key: String = Password::with_theme(theme)
            .with_prompt("API key (placeholder is fine, e.g. \"ollama\")")
            .allow_empty_password(true)
            .interact()?;
        return Ok(if key.trim().is_empty() {
            None
        } else {
            Some(key)
        });
    }

    let key: String = Password::with_theme(theme)
        .with_prompt(format!("API key (Press Enter to skip — {auth_hint})"))
        .allow_empty_password(true)
        .interact()?;
    Ok(if key.trim().is_empty() {
        None
    } else {
        Some(key)
    })
}

/// Heuristic for "local" endpoints — used to default-skip the API key
/// prompt and the catalog fetch. Matches loopback, link-local, mDNS
/// `.local`, and the RFC1918 private IPv4 ranges (10/8, 172.16/12,
/// 192.168/16). Hostnames that resolve to private IPs but aren't
/// literally addresses won't be caught — that needs DNS, which is too
/// slow for an interactive prompt.
fn is_local_endpoint(base_url: &str) -> bool {
    let host = extract_host(base_url);
    if host.is_empty() {
        return false;
    }
    if host == "localhost" || host.ends_with(".local") {
        return true;
    }
    // Loopback / unspecified.
    if host == "127.0.0.1" || host == "0.0.0.0" || host == "::1" {
        return true;
    }
    // RFC1918 + link-local IPv4.
    if let Some(octets) = parse_ipv4(host) {
        let [a, b, _, _] = octets;
        if a == 10 || (a == 192 && b == 168) || (a == 172 && (16..=31).contains(&b)) {
            return true;
        }
        if a == 169 && b == 254 {
            return true; // 169.254/16 link-local
        }
        if a == 127 {
            return true; // entire 127/8 loopback
        }
    }
    false
}

/// Pull the bare host (no scheme, no port, no path) out of `base_url`.
/// Lowercased. IPv6 brackets stripped. Returns "" on malformed input.
fn extract_host(base_url: &str) -> &str {
    let s = base_url.trim();
    let after_scheme = s.split_once("://").map(|(_, rest)| rest).unwrap_or(s);
    // Strip path/query: everything before the first '/' (or '?').
    let host_port = after_scheme
        .split(|c| c == '/' || c == '?')
        .next()
        .unwrap_or("");
    // IPv6 in brackets: [::1]:8080.
    if let Some(rest) = host_port.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return &rest[..end];
        }
    }
    // Strip :port for IPv4 / hostname.
    host_port.split(':').next().unwrap_or("")
}

fn parse_ipv4(host: &str) -> Option<[u8; 4]> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut octets = [0u8; 4];
    for (i, part) in parts.iter().enumerate() {
        octets[i] = part.parse().ok()?;
    }
    Some(octets)
}

/// Fetch the provider's `/models` catalog and present a fuzzy picker;
/// fall back to a typed input pre-filled with `default_model` when the
/// fetch is disabled, fails, or returns nothing.
async fn pick_or_input_model(
    theme: &dyn dialoguer::theme::Theme,
    base_url: &str,
    api_key: &str,
    default_model: &str,
    disable_catalog: bool,
) -> Result<String> {
    if disable_catalog {
        let value: String = Input::with_theme(theme)
            .with_prompt("Default model id")
            .default(default_model.to_string())
            .interact_text()?;
        return Ok(value);
    }

    println!("Fetching models from {base_url} …");
    let models = match fetch_models_with_timeout(base_url, api_key).await {
        Ok(list) if !list.is_empty() => list,
        Ok(_) => {
            println!("  (catalog returned no models — typed input)");
            let value: String = Input::with_theme(theme)
                .with_prompt("Default model id")
                .default(default_model.to_string())
                .interact_text()?;
            return Ok(value);
        }
        Err(e) => {
            println!("  (catalog fetch failed: {e} — typed input)");
            let value: String = Input::with_theme(theme)
                .with_prompt("Default model id")
                .default(default_model.to_string())
                .interact_text()?;
            return Ok(value);
        }
    };

    let labels: Vec<String> = models
        .iter()
        .map(|m| {
            if m.context_length > 0 {
                format!("{}  · ctx {}", m.id, m.context_length)
            } else {
                m.id.clone()
            }
        })
        .collect();
    let default_idx = models
        .iter()
        .position(|m| m.id == default_model)
        .unwrap_or(0);
    let pick = FuzzySelect::with_theme(theme)
        .with_prompt(format!("Pick default model ({} available)", models.len()))
        .items(&labels)
        .default(default_idx)
        .interact()?;
    Ok(models[pick].id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_endpoint_detection_matches_rfc1918_and_loopback() {
        let positives = [
            "http://localhost:11434/v1",
            "http://127.0.0.1/v1",
            "http://10.0.5.7/v1",
            "https://10.255.255.255",
            "http://192.168.1.20:8080/v1",
            "http://172.16.4.4/v1",
            "http://172.31.0.1/v1",
            "http://169.254.169.254",
            "http://my-host.local/v1",
            "http://[::1]:8000/v1",
        ];
        for u in positives {
            assert!(is_local_endpoint(u), "{u} should be local");
        }

        let negatives = [
            "https://api.openai.com/v1",
            "https://openrouter.ai/api/v1",
            "https://172.32.0.1/v1", // outside 172.16-31
            "https://192.169.1.1/v1",
            "https://8.8.8.8/v1",
            "https://example.com/v1",
        ];
        for u in negatives {
            assert!(!is_local_endpoint(u), "{u} should NOT be local");
        }
    }

    #[test]
    fn extract_host_strips_scheme_port_and_path() {
        assert_eq!(
            extract_host("https://api.example.com/v1"),
            "api.example.com"
        );
        assert_eq!(extract_host("http://localhost:11434/v1"), "localhost");
        assert_eq!(extract_host("http://[::1]:8080/x"), "::1");
        assert_eq!(extract_host("https://10.0.0.5"), "10.0.0.5");
        assert_eq!(extract_host(""), "");
    }
}

async fn fetch_models_with_timeout(base_url: &str, api_key: &str) -> Result<Vec<ModelInfo>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()?;
    let cache_ttl = Duration::from_secs(0); // bypass cache during interactive setup
    let cat = catalog::for_base_url(client, base_url, api_key, cache_ttl);
    let models = cat.list_models().await.map_err(anyhow::Error::from)?;
    Ok(models)
}

fn profile_exists(dir: &Path, name: &str) -> Result<bool> {
    let cfg = crate::config::Config::load_from_dir(dir).unwrap_or_default();
    Ok(cfg.providers.contains_key(name))
}

fn dialoguer_theme() -> dialoguer::theme::ColorfulTheme {
    dialoguer::theme::ColorfulTheme::default()
}
