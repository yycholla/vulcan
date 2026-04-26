use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

/// Path to the Vulcan config directory (~/.vulcan/)
pub fn vulcan_home() -> PathBuf {
    dirs_or_default()
}

fn dirs_or_default() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".vulcan")
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

    #[serde(default)]
    pub embeddings: EmbeddingsConfig,

    #[serde(default)]
    pub tui: TuiConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TuiConfig {
    /// Selected theme name. Built-ins: "system" (default), "default-light",
    /// "dracula". Unknown names log a warning and fall back to "system".
    #[serde(default = "default_theme_name")]
    pub theme: String,
}

fn default_theme_name() -> String {
    "system".into()
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            theme: default_theme_name(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct EmbeddingsConfig {
    /// Off by default — set true once an embedding endpoint + model
    /// are configured. Tools surface a "not configured" error when
    /// disabled rather than reaching for a remote API the user
    /// didn't ask for.
    #[serde(default)]
    pub enabled: bool,
    /// OpenAI-compatible embeddings endpoint (e.g.
    /// `https://api.openai.com/v1`). Defaults to the same provider
    /// base URL when blank — convenient for OpenRouter / OpenAI users
    /// whose chat and embedding endpoints share an origin.
    #[serde(default)]
    pub base_url: String,
    /// Optional separate API key for the embeddings endpoint.
    /// Falls back to the provider's `api_key` (or `VULCAN_API_KEY`).
    pub api_key: Option<String>,
    /// Embedding model name (e.g. "text-embedding-3-small").
    #[serde(default = "default_embedding_model")]
    pub model: String,
    /// Embedding dimensionality. Used to validate responses.
    #[serde(default = "default_embedding_dim")]
    pub dim: usize,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderDebugMode {
    #[default]
    Off,
    ToolFallback,
    Wire,
}

impl ProviderDebugMode {
    pub fn logs_wire(self) -> bool {
        matches!(self, Self::Wire)
    }

    pub fn logs_tool_fallback(self) -> bool {
        matches!(self, Self::ToolFallback | Self::Wire)
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct ProviderConfig {
    /// Provider type: "openai-compat" (covers OpenRouter, Anthropic, Ollama, etc.)
    #[serde(default = "default_provider_type")]
    pub r#type: String,
    /// Base URL for API (e.g. https://openrouter.ai/api/v1)
    #[serde(default = "default_base_url")]
    pub base_url: String,
    /// API key — can also be set via VULCAN_API_KEY env var
    pub api_key: Option<String>,
    /// Model name (e.g. "anthropic/claude-sonnet-4", "gpt-4o")
    #[serde(default = "default_model")]
    pub model: String,
    /// Max context size in tokens
    #[serde(default = "default_max_context")]
    pub max_context: usize,
    /// Max retries on transient API failures (429, 5xx, connection errors).
    /// Backoff is exponential with jitter: 1s, 2s, 4s, 8s, 16s.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// How long to cache the provider's `/models` catalog before re-fetching.
    /// Defaults to 24 hours. Set to 0 to disable caching (always fetch fresh).
    #[serde(default = "default_catalog_cache_ttl_hours")]
    pub catalog_cache_ttl_hours: u64,
    /// Skip catalog fetching at startup. Useful when testing or working
    /// offline. Errors from missing or unreachable catalogs are non-fatal
    /// regardless; this just avoids the extra HTTP roundtrip on launch.
    #[serde(default)]
    pub disable_catalog: bool,
    /// Provider protocol debugging:
    /// - "off": no extra provider logging
    /// - "tool-fallback": log raw assistant content when content-shaped tool
    ///   fallback parsing is used
    /// - "wire": also log request and raw response bodies
    #[serde(default)]
    pub debug: ProviderDebugMode,
    /// Max agent loop iterations per prompt. 0 = unlimited (default).
    /// When the agent hits this limit without a text-only response, the
    /// turn ends with a "reached maximum iteration limit" message.
    #[serde(default)]
    pub max_iterations: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ToolsConfig {
    /// Enable dangerous tools (file overwrite, shell exec) without confirmation.
    /// Legacy alias — when true, the `approval.default` falls back to
    /// `always` (YYC-76).
    #[serde(default)]
    pub yolo_mode: bool,
    /// Per-tool approval modes (YYC-76). When unset, every tool runs
    /// without prompting (matches pre-YYC-76 behavior).
    #[serde(default)]
    pub approval: ApprovalConfig,
}

/// Per-tool approval gate (YYC-76).
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalMode {
    /// Block on every invocation, prompting the user.
    Ask,
    /// Pause once per session; subsequent calls run silently.
    Session,
    /// Run without prompting (default).
    #[default]
    Always,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct ApprovalConfig {
    /// Default mode for tools not in the per-tool overrides map.
    /// Defaults to `always` (no prompts) so the gate is opt-in.
    #[serde(default)]
    pub default: ApprovalMode,
    /// Per-tool overrides keyed by tool name (`write_file`, `bash`, etc).
    #[serde(flatten)]
    pub per_tool: std::collections::HashMap<String, ApprovalMode>,
}

impl ApprovalConfig {
    pub fn mode_for(&self, tool: &str) -> ApprovalMode {
        self.per_tool
            .get(tool)
            .copied()
            .unwrap_or(self.default)
    }
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
            max_retries: default_max_retries(),
            catalog_cache_ttl_hours: default_catalog_cache_ttl_hours(),
            disable_catalog: false,
            debug: ProviderDebugMode::Off,
            max_iterations: 0,
        }
    }
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            yolo_mode: false,
            approval: ApprovalConfig::default(),
        }
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
            embeddings: EmbeddingsConfig::default(),
            tui: TuiConfig::default(),
        }
    }
}

impl Default for EmbeddingsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: String::new(),
            api_key: None,
            model: default_embedding_model(),
            dim: default_embedding_dim(),
        }
    }
}

fn default_embedding_model() -> String {
    "text-embedding-3-small".into()
}
fn default_embedding_dim() -> usize {
    1536
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
fn default_max_retries() -> u32 {
    4
}
fn default_catalog_cache_ttl_hours() -> u64 {
    24
}
fn default_skills_dir() -> PathBuf {
    vulcan_home().join("skills")
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
    /// Load config from ~/.vulcan/config.toml, then checks project dir as fallback.
    pub fn load() -> Result<Self> {
        let primary = vulcan_home().join("config.toml");

        // Check multiple locations in order of precedence
        let candidates = [
            ("~/.vulcan/config.toml", primary.clone()),
            ("./config.toml", PathBuf::from("config.toml")),
        ];

        for (label, path) in &candidates {
            if path.exists() {
                let content = std::fs::read_to_string(path).with_context(|| {
                    format!("Failed to read config at {label} ({})", path.display())
                })?;
                let config: Config =
                    toml::from_str(&content).context("Failed to parse config.toml")?;
                tracing::info!("Loaded config from {}", path.display());
                return Ok(config);
            }
        }

        tracing::info!(
            "No config found at ~/.vulcan/config.toml or ./config.toml, using defaults. \
             Copy config.example.toml to ~/.vulcan/config.toml and set your API key."
        );
        Ok(Config::default())
    }

    /// Resolve the API key: env var > config > compile-time warning
    pub fn api_key(&self) -> Option<String> {
        std::env::var("VULCAN_API_KEY")
            .ok()
            .or_else(|| self.provider.api_key.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_debug_mode_parses_from_toml() {
        let config: Config = toml::from_str(
            r#"
[provider]
debug = "wire"
"#,
        )
        .expect("config should parse");

        assert!(matches!(config.provider.debug, ProviderDebugMode::Wire));
    }

    #[test]
    fn provider_debug_mode_helpers_match_expected_scopes() {
        assert!(!ProviderDebugMode::Off.logs_wire());
        assert!(!ProviderDebugMode::Off.logs_tool_fallback());

        assert!(!ProviderDebugMode::ToolFallback.logs_wire());
        assert!(ProviderDebugMode::ToolFallback.logs_tool_fallback());

        assert!(ProviderDebugMode::Wire.logs_wire());
        assert!(ProviderDebugMode::Wire.logs_tool_fallback());
    }
}
