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

/// YYC-156: retention window for migration backup files. Backups
/// older than this are eligible for cleanup by
/// `prune_stale_bak_files`. 30 days is generous enough that an
/// operator who notices a config issue weeks later can still roll
/// back, while preventing indefinite accumulation of stale `.bak`
/// files in `~/.vulcan/`.
pub const BAK_RETENTION_SECS: u64 = 30 * 24 * 60 * 60;

/// YYC-156: known config backup file basenames. Anything matching
/// these gets retention-checked; arbitrary `.bak` files in the dir
/// are left alone. Keeping the set explicit avoids deleting backups
/// the user manually staged for some other reason.
const KNOWN_BAK_FILES: &[&str] = &["config.toml.bak", "keybinds.toml.bak", "providers.toml.bak"];

/// YYC-156: remove known config backup files (`config.toml.bak`,
/// `keybinds.toml.bak`, `providers.toml.bak`) in `dir` whose
/// modified time is older than `retention`. Returns the number of
/// files pruned. Errors stat'ing or removing individual files are
/// logged and skipped — the caller continues with whatever was
/// reachable.
pub fn prune_stale_bak_files(dir: &Path, retention: std::time::Duration) -> usize {
    let now = std::time::SystemTime::now();
    let mut pruned = 0;
    for name in KNOWN_BAK_FILES {
        let bak = dir.join(name);
        let metadata = match std::fs::metadata(&bak) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified = match metadata.modified() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let age = match now.duration_since(modified) {
            Ok(a) => a,
            // Future-dated backup (clock skew) — leave alone.
            Err(_) => continue,
        };
        if age > retention {
            match std::fs::remove_file(&bak) {
                Ok(()) => {
                    tracing::info!(
                        target: "config",
                        path = %bak.display(),
                        age_secs = age.as_secs(),
                        "pruned stale config backup",
                    );
                    pruned += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        target: "config",
                        path = %bak.display(),
                        error = %e,
                        "failed to prune stale config backup",
                    );
                }
            }
        }
    }
    pruned
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

/// Path to the Vulcan config directory (defaults to `~/.vulcan/`).
///
/// Honors the `VULCAN_HOME` environment variable when set — this is the
/// override used by the daemon e2e harness (and by anyone who needs an
/// alternate config root, e.g. tests, CI sandboxing). When unset, falls
/// back to `$HOME/.vulcan`.
pub fn vulcan_home() -> PathBuf {
    if let Ok(override_path) = std::env::var("VULCAN_HOME") {
        if !override_path.is_empty() {
            return PathBuf::from(override_path);
        }
    }
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

    /// YYC-264: self-organizing graph memory via cortex-memory-core.
    /// When enabled, Vulcan stores facts, decisions, and patterns in a
    /// temporal knowledge graph with HNSW vector search, auto-linking,
    /// and briefing synthesis. Off by default — opt in with `enabled = true`.
    /// The database lives at `~/.vulcan/cortex.redb`.
    #[serde(default)]
    pub cortex: CortexConfig,

    /// YYC-17: scheduled jobs the gateway scheduler enqueues into
    /// the inbound queue at their cron firing times. Empty in the
    /// default config; jobs are parsed but their semantics are
    /// validated lazily so the rest of the runtime is unaffected.
    #[serde(default)]
    pub scheduler: SchedulerConfig,

    /// YYC-182: per-workspace trust profile rules. Empty by
    /// default — unknown workspaces fall back to the conservative
    /// `untrusted` level.
    #[serde(default)]
    pub workspace_trust: crate::trust::WorkspaceTrustConfig,

    /// YYC-165 PR-3: per-extension activation policy. Empty by
    /// default — every registered extension activates per its
    /// own status. Operators add ids to `disabled` (or set
    /// `[extensions.<id>] enabled = false`) to force `Inactive`.
    #[serde(default)]
    pub extensions: ExtensionsConfig,

    /// YYC-239 (YYC-238 PR-1): name of the active provider
    /// profile. When set + present in `providers`, both TUI and
    /// gateway resolve their starting provider from
    /// `[providers.<name>]` instead of `[provider]`. `None` →
    /// legacy `[provider]` block (today's default).
    #[serde(default)]
    pub active_profile: Option<String>,

    /// YYC-216: knowledge governance — exclusions that bar
    /// sensitive paths from being indexed by the embeddings
    /// store, code-graph, or any future retrieval layer.
    #[serde(default)]
    pub knowledge: KnowledgeConfig,
}

/// YYC-216: knowledge index governance. Today the only field is
/// `exclusions`, a list of glob patterns that the indexer skips at
/// reindex time so sensitive files (e.g. `.env`, `*.pem`,
/// `secrets/`) never enter the embedding/code-graph DBs in the
/// first place. Per-workspace overrides land alongside the trust
/// profile in a follow-up.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct KnowledgeConfig {
    /// Glob patterns matched against the path *relative to the
    /// workspace root*. Standard `globset` syntax — `*` matches a
    /// single path segment, `**` matches multiple, `?` matches one
    /// character. Lines starting with `#` aren't supported here
    /// (this is a TOML array, not a `.gitignore` file).
    #[serde(default)]
    pub exclusions: Vec<String>,
}

impl KnowledgeConfig {
    /// Compile the exclusion patterns into a `GlobSet` for matching
    /// during indexing. Invalid patterns are logged and skipped so
    /// one typo doesn't stop the whole indexer.
    pub fn build_excluder(&self) -> globset::GlobSet {
        let mut builder = globset::GlobSetBuilder::new();
        for pat in &self.exclusions {
            match globset::Glob::new(pat) {
                Ok(g) => {
                    builder.add(g);
                }
                Err(e) => {
                    tracing::warn!("knowledge.exclusions: invalid glob `{pat}`: {e}");
                }
            }
        }
        builder
            .build()
            .unwrap_or_else(|_| globset::GlobSet::empty())
    }
}

impl Config {
    /// YYC-239: resolve the provider config the agent should boot
    /// against. Honors `active_profile` when set + present in
    /// `providers`; otherwise falls back to the legacy `[provider]`
    /// block. Logs a warning when `active_profile` names a profile
    /// that isn't declared so misconfiguration surfaces at
    /// startup instead of silently using the wrong provider.
    pub fn active_provider_config(&self) -> &ProviderConfig {
        if let Some(name) = self.active_profile.as_deref() {
            if let Some(profile) = self.providers.get(name) {
                return profile;
            }
            tracing::warn!(
                "active_profile = `{name}` not found in [providers]; falling back to [provider]"
            );
        }
        &self.provider
    }
}

/// YYC-165 PR-3: top-level config gate for extensions.
///
/// Wire shape:
///
/// ```toml
/// [extensions]
/// disabled = ["legacy-foo"]      # quick block-list
///
/// [extensions.lint-helper]
/// enabled = true                 # explicit per-id override
/// ```
///
/// `apply_to_registry` returns the count of extensions whose
/// status it flipped to `Inactive` so the caller can log the
/// effect at startup.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct ExtensionsConfig {
    /// Hard block-list. Any extension whose id appears here is
    /// forced to `Inactive` regardless of its own preference.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Per-extension overrides. Settings beyond `enabled` are
    /// preserved but not interpreted at this layer — PR-5 lands
    /// `ConfigField` integration so per-extension settings flow
    /// through `vulcan config`.
    #[serde(default, flatten)]
    pub per_extension: HashMap<String, ExtensionEntryConfig>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct ExtensionEntryConfig {
    /// `Some(true)` activates, `Some(false)` deactivates,
    /// `None` defers to the registry's prior status.
    #[serde(default)]
    pub enabled: Option<bool>,
}

impl ExtensionsConfig {
    /// Apply gating to a registry: any disabled id is forced to
    /// `Inactive`; any explicitly-enabled id has its status
    /// promoted from `Inactive` to `Active`. Returns the number
    /// of extensions whose status flipped.
    pub fn apply_to_registry(&self, registry: &crate::extensions::ExtensionRegistry) -> usize {
        let mut flips = 0usize;
        for id in &self.disabled {
            if registry.set_status(id, crate::extensions::ExtensionStatus::Inactive) {
                flips += 1;
            }
        }
        for (id, entry) in &self.per_extension {
            if self.disabled.contains(id) {
                continue;
            }
            match entry.enabled {
                Some(true) => {
                    if registry.set_status(id, crate::extensions::ExtensionStatus::Active) {
                        flips += 1;
                    }
                }
                Some(false) => {
                    if registry.set_status(id, crate::extensions::ExtensionStatus::Inactive) {
                        flips += 1;
                    }
                }
                None => {}
            }
        }
        flips
    }
}

/// YYC-17: top-level scheduler configuration. Each job entry
/// declares a cron expression, a target platform/lane, and the
/// prompt to fire on schedule. Jobs run only when the gateway is
/// enabled; the TUI / one-shot paths ignore them.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct SchedulerConfig {
    #[serde(default)]
    pub jobs: Vec<SchedulerJobConfig>,
}

/// YYC-17: declarative job spec. `id` is operator-supplied so jobs
/// survive config edits/reorders; `name` is the human label that
/// shows up in tracing. The cron expression is parsed at startup
/// and any failure surfaces as a hard validation error before the
/// gateway binds a listener.
#[derive(Debug, Deserialize, Clone)]
pub struct SchedulerJobConfig {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_scheduler_job_enabled")]
    pub enabled: bool,
    pub cron: String,
    #[serde(default = "default_scheduler_job_timezone")]
    pub timezone: String,
    pub platform: String,
    pub lane: String,
    pub prompt: String,
    /// Hard wall-clock cap on a single job run, in seconds. `None`
    /// means no cap; the run finishes whenever the agent finishes.
    #[serde(default)]
    pub max_runtime_secs: Option<u64>,
    /// Policy when a job's previous run is still active when its
    /// next firing arrives. Default `skip` matches the design doc
    /// recommendation.
    #[serde(default)]
    pub overlap_policy: OverlapPolicy,
}

/// YYC-17: how the scheduler should handle a firing whose previous
/// run is still active.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum OverlapPolicy {
    /// Skip the new firing, leave the previous run alone.
    #[default]
    Skip,
    /// Enqueue anyway — both runs proceed in parallel.
    Enqueue,
    /// Drop any pending firings and replace with the new one.
    Replace,
}

fn default_scheduler_job_enabled() -> bool {
    true
}

fn default_scheduler_job_timezone() -> String {
    "UTC".into()
}

impl SchedulerConfig {
    /// YYC-17: validate every job's cron expression and timezone
    /// before the gateway runtime touches them. Returns the first
    /// actionable error so operators see the bad job by id.
    pub fn validate(&self) -> Result<()> {
        for job in &self.jobs {
            job.validate()
                .with_context(|| format!("scheduler job '{}'", job.id))?;
        }
        Ok(())
    }
}

impl SchedulerJobConfig {
    /// YYC-17: surface bad cron expressions, unknown timezones,
    /// empty platform/lane/prompt, and zero max_runtime_secs as
    /// hard validation errors.
    pub fn validate(&self) -> Result<()> {
        use std::str::FromStr;
        if self.id.trim().is_empty() {
            anyhow::bail!("id is required");
        }
        if self.platform.trim().is_empty() {
            anyhow::bail!("platform is required");
        }
        if self.lane.trim().is_empty() {
            anyhow::bail!("lane is required");
        }
        if self.prompt.trim().is_empty() {
            anyhow::bail!("prompt is required");
        }
        // cron 0.15: parses 6- or 7-field expressions (with seconds).
        cron::Schedule::from_str(&self.cron)
            .with_context(|| format!("invalid cron expression `{}`", self.cron))?;
        chrono_tz::Tz::from_str(&self.timezone)
            .map_err(|e| anyhow::anyhow!("invalid timezone `{}`: {e}", self.timezone))?;
        if matches!(self.max_runtime_secs, Some(0)) {
            anyhow::bail!("max_runtime_secs must be > 0 when set");
        }
        Ok(())
    }
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

/// YYC-264: configuration for embedded cortex-memory-core graph memory.
#[derive(Debug, Deserialize, Clone)]
pub struct CortexConfig {
    /// Master switch. Off by default — flip to `true` to enable.
    #[serde(default)]
    pub enabled: bool,

    /// Path to the cortex.redb database. Defaults to `~/.vulcan/cortex.redb`.
    #[serde(default)]
    pub db_path: Option<PathBuf>,

    /// Embedding model identifier passed to fastembed. Default:
    /// `BAAI/bge-small-en-v1.5` (~30 MB download on first use).
    /// Alternatives: `BAAI/bge-base-en-v1.5`, `BAAI/bge-large-en-v1.5`.
    #[serde(default = "default_cortex_embedding_model")]
    pub embedding_model: String,

    /// Max results returned from a semantic search. Higher values give
    /// broader context at higher token cost.
    #[serde(default = "default_cortex_max_search")]
    pub max_search_results: usize,

    /// Minimum node importance (0.0-1.0) for auto-storage. Nodes below
    /// this threshold are not automatically persisted. Manual stores
    /// always pass through.
    #[serde(default = "default_cortex_min_importance")]
    pub min_importance: f32,
}

impl Default for CortexConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            db_path: None,
            embedding_model: default_cortex_embedding_model(),
            max_search_results: default_cortex_max_search(),
            min_importance: default_cortex_min_importance(),
        }
    }
}

impl CortexConfig {
    /// Returns a config with `enabled: true` and default values.
    /// Used by CLI commands (`vulcan cortex ...`) that always need the
    /// store, regardless of the agent's runtime setting.
    pub fn default_enabled() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }
}

fn default_cortex_embedding_model() -> String {
    "BAAI/bge-small-en-v1.5".into()
}
fn default_cortex_max_search() -> usize {
    5
}
fn default_cortex_min_importance() -> f32 {
    0.3
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

impl GatewayConfig {
    /// YYC-145: validate required fields before the gateway binds a
    /// listener or opens a database. Returns the first actionable error
    /// found; callers should surface the message verbatim.
    pub fn validate(&self) -> Result<()> {
        if self.api_token.trim().is_empty() {
            anyhow::bail!(
                "[gateway] api_token is empty; set a non-empty bearer token in config.toml"
            );
        }
        if self.bind.trim().is_empty() {
            anyhow::bail!("[gateway] bind is empty; set e.g. bind = \"127.0.0.1:7777\"");
        }
        if self.idle_ttl_secs == 0 {
            anyhow::bail!("[gateway] idle_ttl_secs must be > 0");
        }
        if self.max_concurrent_lanes == 0 {
            anyhow::bail!("[gateway] max_concurrent_lanes must be > 0");
        }
        if self.outbound_max_attempts == 0 {
            anyhow::bail!("[gateway] outbound_max_attempts must be > 0");
        }
        if self.discord.enabled && self.discord.bot_token.trim().is_empty() {
            anyhow::bail!(
                "[gateway.discord] enabled = true but bot_token is empty; set bot_token or disable"
            );
        }
        if self.telegram.enabled {
            if self.telegram.bot_token.trim().is_empty() {
                anyhow::bail!(
                    "[gateway.telegram] enabled = true but bot_token is empty; set bot_token or disable"
                );
            }
            if self.telegram.poll_interval_secs > 50 {
                anyhow::bail!("[gateway.telegram] poll_interval_secs must be <= 50 (Telegram cap)");
            }
        }
        Ok(())
    }
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
    /// YYC-19: guild ids the bot will respond in. Empty = open (all
    /// guilds the bot has been invited to). When set, messages from
    /// any other guild are dropped before they hit the inbound
    /// queue. DM messages (no guild) are always allowed.
    #[serde(default)]
    pub allowed_guild_ids: Vec<u64>,
    /// YYC-19: channel ids (including thread channels) the bot
    /// will respond in. Empty = open. When both
    /// `allowed_guild_ids` and `allowed_channel_ids` are set, both
    /// filters must pass.
    #[serde(default)]
    pub allowed_channel_ids: Vec<u64>,
    /// YYC-19: when true, in guild channels (not DMs) the bot only
    /// responds to messages that mention it. Prevents the bot from
    /// reacting to every message in busy channels. DMs always pass —
    /// addressing the bot in a DM IS the mention.
    #[serde(default)]
    pub require_mention: bool,
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
    /// YYC-147: bounded mpsc capacity for the provider stream
    /// channel. The historical default is 1024 — generous enough that
    /// typical bursts never block, small enough that a stuck consumer
    /// surfaces as backpressure within seconds. Tuneable for slow
    /// renderers (raise it) or memory-constrained hosts (lower it).
    /// Clamped to [16, 65536] at read time to avoid pathological
    /// settings.
    #[serde(default = "default_stream_channel_capacity")]
    pub stream_channel_capacity: usize,
}

/// Clamp on `ProviderConfig::stream_channel_capacity` (YYC-147).
/// 16 is small enough to surface backpressure on a stalled consumer;
/// 65536 is well past the point where unbounded growth is the real
/// problem. The clamp is applied at read time via `effective_stream_channel_capacity`.
pub const STREAM_CHANNEL_CAPACITY_MIN: usize = 16;
pub const STREAM_CHANNEL_CAPACITY_MAX: usize = 65_536;
pub const STREAM_CHANNEL_CAPACITY_DEFAULT: usize = 1024;

fn default_stream_channel_capacity() -> usize {
    STREAM_CHANNEL_CAPACITY_DEFAULT
}

impl ProviderConfig {
    /// YYC-147: clamped stream channel capacity for this provider.
    /// Anything outside `[STREAM_CHANNEL_CAPACITY_MIN,
    /// STREAM_CHANNEL_CAPACITY_MAX]` is pulled into range so a
    /// misconfigured value can't OOM the host or starve the
    /// renderer.
    pub fn effective_stream_channel_capacity(&self) -> usize {
        self.stream_channel_capacity
            .clamp(STREAM_CHANNEL_CAPACITY_MIN, STREAM_CHANNEL_CAPACITY_MAX)
    }
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
    /// YYC-181: default tool capability profile applied at session
    /// start. CLI `--profile <name>` overrides. Built-in names:
    /// `readonly`, `coding`, `reviewer`, `gateway-safe`. User-defined
    /// names from [`profiles`](Self::profiles) take precedence over
    /// built-ins on collision.
    #[serde(default)]
    pub profile: Option<String>,
    /// YYC-181: user-defined tool capability profiles. Keyed by
    /// profile name; each value lists the allowed tool names.
    #[serde(default)]
    pub profiles: HashMap<String, ToolProfileConfig>,
}

/// YYC-181: user-defined tool capability profile (config-side).
/// Resolves to a [`crate::tools::ToolProfile`] at runtime.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct ToolProfileConfig {
    /// Optional human-readable description shown in `doctor` output.
    #[serde(default)]
    pub description: String,
    /// Tool names this profile allows. Tools the running registry
    /// doesn't have (extensions, optional features, MCP) are silently
    /// dropped on apply.
    pub allowed: Vec<String>,
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

impl ToolProfileConfig {
    /// YYC-181: convert a config-side profile to the runtime
    /// [`crate::tools::ToolProfile`] under `name`.
    pub fn into_tool_profile(self, name: &str) -> crate::tools::ToolProfile {
        crate::tools::ToolProfile {
            name: name.to_string().into(),
            description: self.description.into(),
            allowed: self.allowed.into_iter().map(Into::into).collect(),
        }
    }
}

impl ToolsConfig {
    /// YYC-181: resolve a profile name (CLI flag or `tools.profile`
    /// in config) to a runtime profile. User-defined profiles in
    /// `[tools.profiles]` shadow the built-in catalog so operators
    /// can replace `coding` etc. with a tighter set.
    pub fn resolve_profile(&self, name: &str) -> Option<crate::tools::ToolProfile> {
        if let Some(custom) = self.profiles.get(name) {
            return Some(custom.clone().into_tool_profile(name));
        }
        crate::tools::builtin_profile(name)
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
            stream_channel_capacity: default_stream_channel_capacity(),
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
            cortex: CortexConfig::default(),
            scheduler: SchedulerConfig::default(),
            workspace_trust: crate::trust::WorkspaceTrustConfig::default(),
            extensions: ExtensionsConfig::default(),
            active_profile: None,
            knowledge: KnowledgeConfig::default(),
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
    /// YYC-161: scan the raw TOML for top-level keys outside the
    /// known set so misspelled sections (`[recal]` instead of
    /// `[recall]`) surface a startup warning rather than silently
    /// reverting to defaults. Returns a sorted, deduplicated list of
    /// unknown keys; an empty Vec is the happy path.
    ///
    /// Conservative on purpose: nested-section validation is left to
    /// serde because adding nested allowlists would have to be
    /// updated every time a sub-struct gains a field, and forward
    /// compatibility with future sections would suffer.
    pub fn detect_unknown_top_level_keys(raw: &str) -> Vec<String> {
        const KNOWN: &[&str] = &[
            "provider",
            "providers",
            "tools",
            "skills_dir",
            "auto_create_skills",
            "compaction",
            "embeddings",
            "tui",
            "gateway",
            "keybinds",
            "recall",
            "scheduler",
            "workspace_trust",
            "extensions",
            "active_profile",
            "knowledge",
        ];
        let Ok(value) = toml::from_str::<toml::Value>(raw) else {
            return Vec::new();
        };
        let Some(table) = value.as_table() else {
            return Vec::new();
        };
        let mut unknown: Vec<String> = table
            .keys()
            .filter(|k| !KNOWN.contains(&k.as_str()))
            .cloned()
            .collect();
        unknown.sort();
        unknown.dedup();
        unknown
    }

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
            // YYC-161: warn on unknown top-level keys before parsing
            // into the strongly-typed `Config`. Default-heavy serde
            // would otherwise silently drop typos (e.g.
            // `[dangerous_commands]` instead of
            // `[tools.dangerous_commands]`) onto unused config keys.
            for key in Self::detect_unknown_top_level_keys(&raw) {
                tracing::warn!(
                    "config.toml: unknown top-level key `{key}` ignored. Did you mean a nested section like `[tools.{key}]`? See config.example.toml for the canonical layout.",
                );
            }
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

        if result.is_ok() {
            // YYC-156: opportunistically prune stale .bak files now
            // that we've snapshotted a fresh one. The retention
            // window protects recent rollback intent; older backups
            // from prior migrations no longer have a meaningful
            // restore target.
            let _ = prune_stale_bak_files(dir, std::time::Duration::from_secs(BAK_RETENTION_SECS));
        }

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

    /// Resolve the API key: env var > active provider > compile-time warning.
    /// YYC-239: pulls the key from the active provider profile (via
    /// `active_provider_config`) instead of always the legacy
    /// `[provider]` block.
    pub fn api_key(&self) -> Option<String> {
        self.api_key_for(self.active_provider_config())
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

// YYC-265: tests live in their own file so the main config module
// stays a manageable size. Same `super::*` access pattern as before;
// the `#[cfg(test)] mod tests;` declaration pulls them in only for
// test builds.
#[cfg(test)]
mod tests;
