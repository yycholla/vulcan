use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

/// Path to the ferris config directory (~/.ferris/)
pub fn ferris_home() -> PathBuf {
    dirs_or_default()
}

fn dirs_or_default() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".ferris")
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    #[serde(default)]
    pub provider: ProviderConfig,

    #[serde(default)]
    pub tools: ToolsConfig,

    #[serde(default = "default_skills_dir")]
    pub skills_dir: PathBuf,

    #[serde(default)]
    pub compaction: CompactionConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ProviderConfig {
    /// Provider type: "openai-compat" (covers OpenRouter, Anthropic, Ollama, etc.)
    #[serde(default = "default_provider_type")]
    pub r#type: String,
    /// Base URL for API (e.g. https://openrouter.ai/api/v1)
    #[serde(default = "default_base_url")]
    pub base_url: String,
    /// API key — can also be set via FERRIS_API_KEY env var
    pub api_key: Option<String>,
    /// Model name (e.g. "anthropic/claude-sonnet-4", "gpt-4o")
    #[serde(default = "default_model")]
    pub model: String,
    /// Max context size in tokens
    #[serde(default = "default_max_context")]
    pub max_context: usize,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ToolsConfig {
    /// Enable dangerous tools (file overwrite, shell exec) without confirmation
    #[serde(default)]
    pub yolo_mode: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct CompactionConfig {
    /// Enable automatic context compression
    #[serde(default = "default_compaction_enabled")]
    pub enabled: bool,
    /// Token ratio threshold to trigger compaction (0.0 - 1.0)
    #[serde(default = "default_trigger_ratio")]
    pub trigger_ratio: f64,
    /// Reserved tokens for LLM response
    #[serde(default = "default_reserved_tokens")]
    pub reserved_tokens: usize,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            r#type: default_provider_type(),
            base_url: default_base_url(),
            api_key: None,
            model: default_model(),
            max_context: default_max_context(),
        }
    }
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self { yolo_mode: false }
    }
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            enabled: default_compaction_enabled(),
            trigger_ratio: default_trigger_ratio(),
            reserved_tokens: default_reserved_tokens(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: ProviderConfig::default(),
            tools: ToolsConfig::default(),
            skills_dir: default_skills_dir(),
            compaction: CompactionConfig::default(),
        }
    }
}

fn default_provider_type() -> String {
    "openai-compat".into()
}
fn default_base_url() -> String {
    "https://openrouter.ai/api/v1".into()
}
fn default_model() -> String {
    "deepseek/deepseek-v4-flash".into()
}
fn default_max_context() -> usize {
    128_000
}
fn default_skills_dir() -> PathBuf {
    ferris_home().join("skills")
}
fn default_compaction_enabled() -> bool {
    true
}
fn default_trigger_ratio() -> f64 {
    0.85
}
fn default_reserved_tokens() -> usize {
    50_000
}

impl Config {
    /// Load config from ~/.ferris/config.toml, then checks project dir as fallback.
    pub fn load() -> Result<Self> {
        let primary = ferris_home().join("config.toml");

        // Check multiple locations in order of precedence
        let candidates = [
            ("~/.ferris/config.toml", primary.clone()),
            ("./config.toml", PathBuf::from("config.toml")),
        ];

        for (label, path) in &candidates {
            if path.exists() {
                let content = std::fs::read_to_string(path)
                    .with_context(|| format!("Failed to read config at {label} ({})", path.display()))?;
                let config: Config =
                    toml::from_str(&content).context("Failed to parse config.toml")?;
                tracing::info!("Loaded config from {}", path.display());
                return Ok(config);
            }
        }

        tracing::info!(
            "No config found at ~/.ferris/config.toml or ./config.toml, using defaults. \
             Copy config.example.toml to ~/.ferris/config.toml and set your API key."
        );
        Ok(Config::default())
    }

    /// Resolve the API key: env var > config > compile-time warning
    pub fn api_key(&self) -> Option<String> {
        std::env::var("FERRIS_API_KEY")
            .ok()
            .or_else(|| self.provider.api_key.clone())
    }
}
