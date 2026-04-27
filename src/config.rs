use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Atomic-write helper for config-fragment files (YYC-136).
///
/// Writes to `<path>.tmp`, fsyncs the data + metadata, then renames
/// over the destination. The rename is atomic on POSIX and on Windows
/// (rename-overwrites-existing semantics on Win10+). A mid-write
/// crash leaves the destination either fully old or fully new — never
/// a truncated middle state.
pub(crate) fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let file_name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        anyhow::anyhow!("atomic_write: path has no file name: {}", path.display())
    })?;
    let tmp = path.with_file_name(format!("{file_name}.tmp"));
    {
        let mut f = std::fs::File::create(&tmp)
            .with_context(|| format!("Failed to create {}", tmp.display()))?;
        f.write_all(content.as_bytes())
            .with_context(|| format!("Failed to write {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("Failed to fsync {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).with_context(|| {
        format!(
            "Failed to atomic-rename {} → {}",
            tmp.display(),
            path.display()
        )
    })?;
    Ok(())
}

/// Snapshot `path` to `<path>.bak` and return the backup's path.
/// Used by `Config::migrate` so a failed migration can roll the
/// original file back into place (YYC-136). Caller is expected to
/// only call this when `path` exists.
pub(crate) fn snapshot_bak(path: &Path) -> Result<PathBuf> {
    let file_name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        anyhow::anyhow!("snapshot_bak: path has no file name: {}", path.display())
    })?;
    let bak = path.with_file_name(format!("{file_name}.bak"));
    std::fs::copy(path, &bak).with_context(|| {
        format!(
            "Failed to snapshot {} to {} (config migration safety net)",
            path.display(),
            bak.display(),
        )
    })?;
    Ok(bak)
}

/// Outcome of a `Config::migrate(dir, force)` run (YYC-99). Booleans
/// flag which fragment files the run produced so the CLI can print a
/// honest summary.
#[derive(Debug, Default, Clone, Copy)]
pub struct MigrationReport {
    pub keybinds_written: bool,
    pub providers_written: bool,
    pub main_rewritten: bool,
}

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

    /// Additional named provider profiles. The legacy `[provider]` table
    /// remains the active profile for now; named profiles give config a place
    /// to bind auth/base URL/model together before provider switching grows a
    /// dedicated UI.
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,

    #[serde(default)]
    pub tools: ToolsConfig,

    #[serde(default = "default_skills_dir")]
    pub skills_dir: PathBuf,

    /// YYC-20: when true, after 5+ iterations the agent asks the
    /// active provider to summarize the turn as a draft skill and
    /// writes it to `<skills_dir>/_pending/<name>.md` for manual
    /// review. Off by default — opting in burns extra tokens at the
    /// end of long turns.
    #[serde(default)]
    pub auto_create_skills: bool,

    #[serde(default)]
    pub compaction: CompactionConfig,

    #[serde(default)]
    pub embeddings: EmbeddingsConfig,

    #[serde(default)]
    pub tui: TuiConfig,

    #[serde(default)]
    pub gateway: Option<GatewayConfig>,

    #[serde(default)]
    pub keybinds: KeybindsConfig,

    /// YYC-42: auto-inject relevant past-session context on the first
    /// turn of a new session. Off by default — flip to enabled = true
    /// after reviewing the privacy tradeoff (recalled snippets land in
    /// the system prompt and are visible to the model).
    #[serde(default)]
    pub recall: RecallConfig,
}

/// Auto-recall config (YYC-42). Drives the `RecallHook` BeforePrompt
/// handler. When enabled, the first user prompt of a fresh session is
/// run through FTS5 against the messages table and the top hits are
/// injected as a System message at AfterSystem position.
#[derive(Debug, Deserialize, Clone, Copy)]
pub struct RecallConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_recall_max_hits")]
    pub max_hits: usize,
    /// BM25 score cap (FTS5 returns lower-is-better ranks; SQLite's
    /// `bm25()` returns negative numbers where more-negative = closer
    /// match). Hits with `score > min_score` are skipped. Default
    /// includes everything; tighten for a stricter relevance bar.
    #[serde(default = "default_recall_min_score")]
    pub min_score: f64,
    /// Max characters per recalled hit before truncation. Long
    /// historical messages can blow the context budget.
    #[serde(default = "default_recall_max_chars")]
    pub max_chars_per_hit: usize,
}

impl Default for RecallConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_hits: default_recall_max_hits(),
            min_score: default_recall_min_score(),
            max_chars_per_hit: default_recall_max_chars(),
        }
    }
}

fn default_recall_max_hits() -> usize {
    5
}
fn default_recall_min_score() -> f64 {
    0.0
}
fn default_recall_max_chars() -> usize {
    400
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

/// Raw, unparsed key-binding strings the user can override in
/// `~/.vulcan/config.toml`. The TUI parses these into
/// `crate::tui::keybinds::Keybinds` at startup; unparseable values fall
/// back to defaults with a tracing warning, so a typo never silently
/// drops a binding (YYC-90).
#[derive(Debug, Deserialize, Clone)]
pub struct KeybindsConfig {
    #[serde(default = "default_key_sessions")]
    pub toggle_sessions: String,
    #[serde(default = "default_key_tools")]
    pub toggle_tools: String,
    #[serde(default = "default_key_reasoning")]
    pub toggle_reasoning: String,
    #[serde(default = "default_key_cancel")]
    pub cancel: String,
    #[serde(default = "default_key_queue_drop")]
    pub queue_drop: String,
}

impl Default for KeybindsConfig {
    fn default() -> Self {
        Self {
            toggle_sessions: default_key_sessions(),
            toggle_tools: default_key_tools(),
            toggle_reasoning: default_key_reasoning(),
            cancel: default_key_cancel(),
            queue_drop: default_key_queue_drop(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct GatewayConfig {
    #[serde(default = "default_gateway_bind")]
    pub bind: String,
    pub api_token: String,
    #[serde(default = "default_gateway_idle_ttl_secs")]
    pub idle_ttl_secs: u64,
    #[serde(default = "default_gateway_max_concurrent_lanes")]
    pub max_concurrent_lanes: usize,
    #[serde(default = "default_gateway_outbound_max_attempts")]
    pub outbound_max_attempts: u32,
    #[serde(default)]
    pub discord: DiscordConfig,
    /// YYC-18 PR-3: Telegram connector. Behind the `telegram` cargo
    /// feature at the wiring layer; the config struct itself lives
    /// unconditionally so TOML round-trips cleanly regardless of the
    /// feature set the binary was built with.
    #[serde(default)]
    pub telegram: TelegramConfig,
    /// YYC-18 PR-2c: slash commands routed through the gateway worker
    /// before falling through to the streaming agent. Built-ins
    /// (/help, /status, /clear, /resume) are pre-registered by
    /// `CommandDispatcher::new`; entries here add custom commands or
    /// override a builtin (`kind = "shell"` against e.g. `"help"`
    /// replaces the registered builtin).
    #[serde(default)]
    pub commands: HashMap<String, CommandConfig>,
}

/// YYC-18 PR-2c: per-command configuration. Tagged via
/// `serde(tag = "kind")` so a TOML entry reads naturally:
///
/// ```toml
/// [gateway.commands]
/// mybot = { kind = "shell", command = "scripts/mybot.sh" }
/// ```
///
/// `Builtin { name }` is rarely needed in user TOML — the four built-in
/// names are registered automatically. It exists so a config can
/// pin a builtin under a different name if desired.
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandConfig {
    /// Built-in command. `name` selects which handler runs; supported
    /// values: "help", "status", "clear", "resume". Unknown names are
    /// warn-logged at dispatcher build time and skipped.
    Builtin { name: String },
    /// Run a subprocess and reply with its stdout. The user's message
    /// body (after `/<name>`) is piped into stdin, and `VULCAN_PLATFORM`
    /// / `VULCAN_CHAT_ID` / `VULCAN_USER_ID` env vars are set on the
    /// child.
    Shell {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default = "default_command_timeout_secs")]
        timeout_secs: u64,
        #[serde(default)]
        working_dir: Option<std::path::PathBuf>,
    },
}

fn default_command_timeout_secs() -> u64 {
    30
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct DiscordConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bot_token: String,
    #[serde(default)]
    pub allow_bots: bool,
}

/// YYC-18 PR-3: Telegram connector configuration.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct TelegramConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bot_token: String,
    /// When set, the gateway accepts webhook POSTs at
    /// `/webhook/telegram` and verifies the
    /// `X-Telegram-Bot-Api-Secret-Token` header against this value.
    /// Empty means webhooks disabled — gateway only receives via
    /// long-poll.
    #[serde(default)]
    pub webhook_secret: String,
    /// Chat ids allowed to talk to the bot. Empty = open (every chat
    /// the bot is added to is served). Telegram chat ids are i64;
    /// negative for groups, positive for DMs.
    #[serde(default)]
    pub allowed_chat_ids: Vec<i64>,
    /// How many seconds the long-poll `getUpdates` request waits for
    /// new messages. 0 = short-poll (busy loop). Telegram caps at 50;
    /// 25 is a reasonable middle.
    #[serde(default = "default_telegram_poll_interval_secs")]
    pub poll_interval_secs: u32,
}

fn default_telegram_poll_interval_secs() -> u32 {
    25
}

fn default_gateway_bind() -> String {
    "127.0.0.1:7777".into()
}
fn default_gateway_idle_ttl_secs() -> u64 {
    1800
}
fn default_gateway_max_concurrent_lanes() -> usize {
    64
}
fn default_gateway_outbound_max_attempts() -> u32 {
    5
}

fn default_key_sessions() -> String {
    "Ctrl+K".into()
}
fn default_key_tools() -> String {
    "Ctrl+T".into()
}
fn default_key_reasoning() -> String {
    "Ctrl+R".into()
}
fn default_key_cancel() -> String {
    "Ctrl+C".into()
}
fn default_key_queue_drop() -> String {
    "Ctrl+Backspace".into()
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
    /// Cap on `max_tokens` (max output tokens) the provider will produce
    /// for a single response. Provider default if `None` (currently 8096).
    /// Lower this for small-context models where the default would crowd
    /// out room for the prompt; raise it for models with long-form output.
    #[serde(default)]
    pub max_output_tokens: Option<usize>,
}

#[derive(Debug, Deserialize, Clone, Default)]
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
    /// How aggressively to redirect bash invocations to native-tool
    /// equivalents (YYC-84 / YYC-89). The `PreferNativeTools` hook
    /// reads this to decide whether to block, warn, or stay out of the
    /// way entirely.
    #[serde(default)]
    pub native_enforcement: NativeEnforcement,
    /// Policy + quota for commands that match a SafetyHook dangerous
    /// pattern (YYC-130 follow-up). Default: `policy = "prompt"` and
    /// `quota_per_session = 5` — match the existing prompt-then-allow
    /// flow, with a fresh per-session cap on how many times an
    /// approved-and-remembered dangerous command can fire before the
    /// hook re-prompts.
    #[serde(default)]
    pub dangerous_commands: DangerousCommandsConfig,
}

/// Approval-flow policy for SafetyHook (YYC-130 follow-up).
///
/// * `Prompt` — pause and ask the user via the existing pause channel
///   (matches the legacy behavior). When no pause channel is wired
///   (CLI one-shot), falls through to a hard block.
/// * `Block`  — never prompt; always hard-block any dangerous command.
///   Useful for unattended runs / CI / production.
/// * `Allow`  — never prompt and never block. Effectively disables the
///   safety hook for dangerous patterns. **Not recommended.**
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum DangerousCommandPolicy {
    #[default]
    Prompt,
    Block,
    Allow,
}

#[derive(Debug, Deserialize, Clone, Copy)]
pub struct DangerousCommandsConfig {
    #[serde(default)]
    pub policy: DangerousCommandPolicy,
    /// Per-session usage cap for any single approved-and-remembered
    /// dangerous command. After this many fires the SafetyHook
    /// re-prompts as if the cache entry had expired. 0 disables the
    /// cap (legacy behavior — once approved, runs unlimited).
    #[serde(default = "default_dangerous_quota")]
    pub quota_per_session: u32,
}

fn default_dangerous_quota() -> u32 {
    5
}

impl Default for DangerousCommandsConfig {
    fn default() -> Self {
        Self {
            policy: DangerousCommandPolicy::default(),
            quota_per_session: default_dangerous_quota(),
        }
    }
}

/// Native-tool redirect aggressiveness.
///
/// * `Off`   — hook disabled.
/// * `Warn`  — log + count via the audit hook (YYC-88), but pass through.
/// * `Block` — return `HookOutcome::Block` with a redirect message (default).
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum NativeEnforcement {
    Off,
    Warn,
    #[default]
    Block,
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
        self.per_tool.get(tool).copied().unwrap_or(self.default)
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
            max_output_tokens: None,
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
            providers: HashMap::new(),
            tools: ToolsConfig::default(),
            skills_dir: default_skills_dir(),
            auto_create_skills: false,
            compaction: CompactionConfig::default(),
            embeddings: EmbeddingsConfig::default(),
            tui: TuiConfig::default(),
            gateway: None,
            keybinds: KeybindsConfig::default(),
            recall: RecallConfig::default(),
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
    /// Load config from `~/.vulcan/`, falling back to project-dir
    /// `./config.toml` for in-repo dev runs.
    ///
    /// YYC-99: the config now lives across three files in the dir:
    /// `config.toml` (main), `keybinds.toml`, `providers.toml`.
    /// Missing files are fine; legacy monolithic `config.toml` blocks
    /// still work and are surfaced via a deprecation log.
    pub fn load() -> Result<Self> {
        let home = vulcan_home();
        if home.join("config.toml").exists()
            || home.join("keybinds.toml").exists()
            || home.join("providers.toml").exists()
        {
            return Self::load_from_dir(&home);
        }

        // Repo-relative fallback for cargo-run dev workflows.
        let proj = std::env::current_dir().ok();
        if let Some(dir) = proj
            && dir.join("config.toml").exists()
        {
            return Self::load_from_dir(&dir);
        }

        tracing::info!(
            "No config found at ~/.vulcan/ or ./config.toml — using defaults. \
             Copy config.example.toml to ~/.vulcan/config.toml and set your API key."
        );
        Ok(Config::default())
    }

    /// Load every config fragment under `dir` (`config.toml` +
    /// `keybinds.toml` + `providers.toml`) and merge into a single
    /// `Config`. Each file is optional. Explicit fragment files take
    /// precedence over the same blocks inlined in the legacy
    /// `config.toml`.
    pub fn load_from_dir(dir: &Path) -> Result<Self> {
        let main_path = dir.join("config.toml");
        let mut config = if main_path.exists() {
            let raw = std::fs::read_to_string(&main_path)
                .with_context(|| format!("Failed to read {}", main_path.display()))?;
            let parsed: Config = toml::from_str(&raw)
                .with_context(|| format!("Failed to parse {}", main_path.display()))?;
            tracing::info!("Loaded main config from {}", main_path.display());
            parsed
        } else {
            Config::default()
        };

        let keybinds_path = dir.join("keybinds.toml");
        if keybinds_path.exists() {
            let raw = std::fs::read_to_string(&keybinds_path)
                .with_context(|| format!("Failed to read {}", keybinds_path.display()))?;
            let kb: KeybindsConfig = toml::from_str(&raw)
                .with_context(|| format!("Failed to parse {}", keybinds_path.display()))?;
            config.keybinds = kb;
            tracing::info!("Loaded keybinds from {}", keybinds_path.display());
        }

        let providers_path = dir.join("providers.toml");
        if providers_path.exists() {
            let raw = std::fs::read_to_string(&providers_path)
                .with_context(|| format!("Failed to read {}", providers_path.display()))?;
            let parsed: HashMap<String, ProviderConfig> = toml::from_str(&raw)
                .with_context(|| format!("Failed to parse {}", providers_path.display()))?;
            // Merge: explicit providers.toml takes precedence; entries
            // also present in `config.toml`'s `[providers.*]` survive
            // unless the same name appears in providers.toml.
            for (name, profile) in parsed {
                config.providers.insert(name, profile);
            }
            tracing::info!("Loaded providers from {}", providers_path.display());
        }

        if main_path.exists() {
            let raw = std::fs::read_to_string(&main_path).unwrap_or_default();
            if raw.contains("[keybinds]") && !keybinds_path.exists() {
                tracing::warn!(
                    "config.toml still contains [keybinds]; consider running \
                     `vulcan migrate-config` to split it into keybinds.toml (YYC-99)."
                );
            }
            if raw.contains("[providers.") && !providers_path.exists() {
                tracing::warn!(
                    "config.toml still contains [providers.<name>] blocks; consider running \
                     `vulcan migrate-config` to split them into providers.toml (YYC-99)."
                );
            }
        }

        Ok(config)
    }

    /// Load a single legacy `config.toml` from a specific path. Used by
    /// `vulcan provider` which writes into a per-fragment file —
    /// callers that just need to read the *main* config block.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("Failed to parse {}", path.display()))
    }

    /// One-shot migration for users still on the monolithic
    /// `config.toml`: extract `[keybinds]` into `keybinds.toml` and
    /// every `[providers.<name>]` into `providers.toml`. Existing
    /// fragment files are not overwritten unless `force` is true.
    /// Idempotent — safe to run repeatedly.
    pub fn migrate(dir: &Path, force: bool) -> Result<MigrationReport> {
        let main_path = dir.join("config.toml");
        if !main_path.exists() {
            return Ok(MigrationReport::default());
        }

        // YYC-136: snapshot the original config to <name>.bak before
        // mutating anything. If any subsequent step errors, restore from
        // the snapshot and remove any half-written .tmp files so the
        // user is left with the same state they started with — never a
        // wedged config.
        let bak_path = snapshot_bak(&main_path)?;

        let result = Self::migrate_inner(dir, force, &main_path);

        if let Err(e) = &result {
            tracing::warn!(
                "config migration failed mid-flight: {e}. Rolling back from {}",
                bak_path.display()
            );
            // Best-effort restore — the user keeps their original
            // config no matter what happens during migration.
            if let Err(restore_err) = std::fs::copy(&bak_path, &main_path) {
                tracing::error!(
                    "config migration rollback also failed: {restore_err}. \
                     Original config is at {}.",
                    bak_path.display()
                );
            }
            // Sweep any lingering .tmp files from atomic_write.
            for name in ["config.toml", "keybinds.toml", "providers.toml"] {
                let tmp = dir.join(format!("{name}.tmp"));
                if tmp.exists() {
                    let _ = std::fs::remove_file(tmp);
                }
            }
        }

        result
    }

    /// Body of `migrate` extracted so the outer function owns the
    /// snapshot / rollback boundary. All file writes here go through
    /// `atomic_write` (write `.tmp`, fsync, rename) so a mid-write
    /// crash leaves the destination either fully old or fully new —
    /// never a half-byte truncation (YYC-136).
    fn migrate_inner(dir: &Path, force: bool, main_path: &Path) -> Result<MigrationReport> {
        let mut report = MigrationReport::default();

        let raw = std::fs::read_to_string(main_path)
            .with_context(|| format!("Failed to read {}", main_path.display()))?;
        let mut doc: toml_edit::DocumentMut = raw
            .parse()
            .with_context(|| format!("Failed to parse {}", main_path.display()))?;

        // ── Keybinds.
        let keybinds_path = dir.join("keybinds.toml");
        if let Some(item) = doc.remove("keybinds") {
            if keybinds_path.exists() && !force {
                tracing::warn!(
                    "keybinds.toml already exists; leaving [keybinds] in config.toml. \
                     Use --force to overwrite."
                );
                doc.insert("keybinds", item);
            } else {
                let table = match item.as_table() {
                    Some(t) => t.clone(),
                    None => anyhow::bail!("[keybinds] in config.toml is not a table"),
                };
                let mut out = toml_edit::DocumentMut::new();
                for (k, v) in table.iter() {
                    out.insert(k, v.clone());
                }
                atomic_write(&keybinds_path, &out.to_string())?;
                report.keybinds_written = true;
            }
        }

        // ── Providers.
        let providers_path = dir.join("providers.toml");
        if let Some(item) = doc.remove("providers") {
            if providers_path.exists() && !force {
                tracing::warn!(
                    "providers.toml already exists; leaving [providers.*] in config.toml. \
                     Use --force to overwrite."
                );
                doc.insert("providers", item);
            } else {
                let table = match item.as_table() {
                    Some(t) => t.clone(),
                    None => anyhow::bail!("[providers] in config.toml is not a table"),
                };
                let mut out = toml_edit::DocumentMut::new();
                for (name, sub) in table.iter() {
                    out.insert(name, sub.clone());
                }
                atomic_write(&providers_path, &out.to_string())?;
                report.providers_written = true;
            }
        }

        if report.keybinds_written || report.providers_written {
            atomic_write(main_path, &doc.to_string())?;
            report.main_rewritten = true;
        }
        Ok(report)
    }

    /// Resolve the API key: env var > config > compile-time warning
    pub fn api_key(&self) -> Option<String> {
        self.api_key_for(&self.provider)
    }

    /// Resolve the API key for a provider profile: env var wins, then the
    /// profile-local key. Named providers intentionally use the same global
    /// env override so one-off shells can redirect auth without editing TOML.
    pub fn api_key_for(&self, provider: &ProviderConfig) -> Option<String> {
        std::env::var("VULCAN_API_KEY")
            .ok()
            .or_else(|| provider.api_key.clone())
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
    fn native_enforcement_round_trips_each_mode() {
        for (raw, expected) in [
            ("off", NativeEnforcement::Off),
            ("warn", NativeEnforcement::Warn),
            ("block", NativeEnforcement::Block),
        ] {
            let toml = format!("[tools]\nnative_enforcement = \"{raw}\"\n");
            let cfg: Config = toml::from_str(&toml).expect("should parse");
            assert_eq!(cfg.tools.native_enforcement, expected);
        }
    }

    #[test]
    fn native_enforcement_defaults_to_block_when_missing() {
        let cfg: Config = toml::from_str("").expect("empty parses");
        assert_eq!(cfg.tools.native_enforcement, NativeEnforcement::Block);
        let cfg: Config = toml::from_str("[tools]\n").expect("empty tools parses");
        assert_eq!(cfg.tools.native_enforcement, NativeEnforcement::Block);
    }

    #[test]
    fn keybinds_block_parses_with_overrides() {
        let config: Config = toml::from_str(
            r#"
[keybinds]
toggle_tools = "F2"
"#,
        )
        .expect("config should parse");

        assert_eq!(config.keybinds.toggle_tools, "F2");
        assert_eq!(config.keybinds.toggle_sessions, "Ctrl+K");
        assert_eq!(config.keybinds.cancel, "Ctrl+C");
    }

    #[test]
    fn keybinds_default_when_section_missing() {
        let config: Config = toml::from_str("").expect("empty toml is valid");
        let defaults = KeybindsConfig::default();
        assert_eq!(config.keybinds.toggle_tools, defaults.toggle_tools);
        assert_eq!(config.keybinds.toggle_sessions, defaults.toggle_sessions);
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

    #[test]
    fn gateway_section_parses_with_defaults() {
        let toml = r#"
            [gateway]
            api_token = "test-token"
        "#;
        let cfg: Config = toml::from_str(toml).expect("parse");
        let g = cfg.gateway.expect("gateway present");
        assert_eq!(g.bind, "127.0.0.1:7777");
        assert_eq!(g.api_token, "test-token");
        assert_eq!(g.idle_ttl_secs, 1800);
        assert_eq!(g.max_concurrent_lanes, 64);
        assert_eq!(g.outbound_max_attempts, 5);
    }

    #[test]
    fn gateway_discord_section_parses_with_defaults() {
        let toml = r#"
            [gateway]
            api_token = "test-token"

            [gateway.discord]
            enabled = true
            bot_token = "discord-token"
        "#;
        let cfg: Config = toml::from_str(toml).expect("parse");
        let discord = cfg.gateway.expect("gateway present").discord;
        assert!(discord.enabled);
        assert_eq!(discord.bot_token, "discord-token");
        assert!(!discord.allow_bots);
    }

    #[test]
    fn named_provider_profiles_parse_without_breaking_legacy_provider() {
        let toml = r#"
            [provider]
            base_url = "https://openrouter.ai/api/v1"
            api_key = "openrouter-key"
            model = "deepseek/deepseek-v4-flash"

            [providers.local]
            base_url = "http://localhost:11434/v1"
            api_key = "ollama-key"
            model = "qwen2.5-coder:latest"
            disable_catalog = true
        "#;

        let cfg: Config = toml::from_str(toml).expect("config should parse");

        assert_eq!(cfg.provider.model, "deepseek/deepseek-v4-flash");
        assert_eq!(cfg.providers["local"].base_url, "http://localhost:11434/v1");
        assert_eq!(
            cfg.providers["local"].api_key.as_deref(),
            Some("ollama-key")
        );
        assert!(cfg.providers["local"].disable_catalog);
    }

    #[test]
    fn load_from_dir_handles_missing_files_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load_from_dir(dir.path()).expect("empty dir → defaults");
        assert!(cfg.providers.is_empty());
        assert_eq!(cfg.keybinds.toggle_tools, "Ctrl+T");
    }

    #[test]
    fn load_from_dir_merges_three_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"
[provider]
type = "openai-compat"
base_url = "https://main.example/v1"
model = "main-1"
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("keybinds.toml"),
            r#"
toggle_tools = "F2"
toggle_sessions = "Ctrl+P"
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("providers.toml"),
            r#"
[local]
type = "openai-compat"
base_url = "http://localhost:11434/v1"
model = "qwen2.5-coder:latest"
disable_catalog = true
"#,
        )
        .unwrap();

        let cfg = Config::load_from_dir(dir.path()).unwrap();
        assert_eq!(cfg.provider.base_url, "https://main.example/v1");
        assert_eq!(cfg.keybinds.toggle_tools, "F2");
        assert_eq!(cfg.keybinds.toggle_sessions, "Ctrl+P");
        assert_eq!(cfg.providers["local"].model, "qwen2.5-coder:latest");
        assert!(cfg.providers["local"].disable_catalog);
    }

    #[test]
    fn migrate_extracts_keybinds_and_providers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"# top comment
[provider]
type = "openai-compat"
base_url = "https://x.example/v1"
model = "x-1"

[keybinds]
toggle_tools = "F4"

[providers.local]
type = "openai-compat"
base_url = "http://localhost:11434/v1"
model = "qwen2.5"
"#,
        )
        .unwrap();

        let report = Config::migrate(dir.path(), false).unwrap();
        assert!(report.keybinds_written);
        assert!(report.providers_written);
        assert!(report.main_rewritten);

        // After split: original config.toml should no longer contain
        // [keybinds] or [providers.*], the fragment files should.
        let main_after = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(!main_after.contains("[keybinds]"));
        assert!(!main_after.contains("[providers"));

        let keybinds_raw = std::fs::read_to_string(dir.path().join("keybinds.toml")).unwrap();
        assert!(keybinds_raw.contains("toggle_tools = \"F4\""));

        let providers_raw = std::fs::read_to_string(dir.path().join("providers.toml")).unwrap();
        assert!(providers_raw.contains("[local]"));

        // Re-run is a no-op (idempotent).
        let report2 = Config::migrate(dir.path(), false).unwrap();
        assert!(!report2.keybinds_written);
        assert!(!report2.providers_written);

        // Round-trip: load the migrated layout and assert behavior matches
        // pre-migration.
        let cfg = Config::load_from_dir(dir.path()).unwrap();
        assert_eq!(cfg.keybinds.toggle_tools, "F4");
        assert_eq!(cfg.providers["local"].model, "qwen2.5");
    }

    // ── YYC-136: atomic write + rollback safety net ─────────────────────

    #[test]
    fn atomic_write_replaces_destination_atomically() {
        // YYC-136: after atomic_write, the destination contains the new
        // content and no .tmp file is left behind.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "old = true\n").unwrap();

        atomic_write(&path, "new = true\n").unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, "new = true\n");
        assert!(!dir.path().join("config.toml.tmp").exists());
    }

    #[test]
    fn migrate_writes_bak_snapshot_for_rollback() {
        // YYC-136: every migration run snapshots the pre-mutation
        // config.toml to config.toml.bak. After the run completes the
        // .bak still exists so the user has a manual undo path.
        let dir = tempfile::tempdir().unwrap();
        let original = "# original\n[keybinds]\ntoggle_tools = \"F4\"\n";
        std::fs::write(dir.path().join("config.toml"), original).unwrap();

        Config::migrate(dir.path(), false).unwrap();

        let bak = dir.path().join("config.toml.bak");
        assert!(bak.exists(), ".bak snapshot should survive migration");
        let bak_raw = std::fs::read_to_string(&bak).unwrap();
        assert_eq!(bak_raw, original);
    }

    #[test]
    fn migrate_rolls_back_when_inner_step_fails() {
        // YYC-136: simulate a failure mid-migration by handing migrate
        // a pre-existing keybinds.toml that's a directory (so the write
        // attempt fails). Without rollback the user would be left with
        // a wedged config; with rollback the original config.toml is
        // restored.
        let dir = tempfile::tempdir().unwrap();
        let original = "[keybinds]\ntoggle_tools = \"F4\"\n";
        std::fs::write(dir.path().join("config.toml"), original).unwrap();

        // Create keybinds.toml as a *directory* — atomic_write will fail
        // when its rename target is a non-empty directory on Linux.
        std::fs::create_dir(dir.path().join("keybinds.toml")).unwrap();
        std::fs::write(
            dir.path().join("keybinds.toml").join("blocker"),
            "non-empty\n",
        )
        .unwrap();

        // force=true so migration tries to overwrite the (directory)
        // keybinds.toml — that's the step that errors.
        let result = Config::migrate(dir.path(), true);
        assert!(
            result.is_err(),
            "expected migration to fail when keybinds.toml is a non-empty dir"
        );

        // Rollback ran: config.toml still has its original content.
        let restored = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert_eq!(
            restored, original,
            "config.toml should be rolled back to the original snapshot"
        );

        // No .tmp leftover from the partial write.
        assert!(!dir.path().join("config.toml.tmp").exists());
    }
}
