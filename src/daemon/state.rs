//! Long-lived daemon process state. Holds the shutdown / reload signals,
//! the SessionMap, and the shared CortexStore (Slice 1).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::RwLock;
use serde::Serialize;
use tokio::sync::watch;

use crate::config::{Config, MigrationReport, vulcan_home};
use crate::daemon::session::SessionMap;
use crate::daemon::session_agent::SessionAgentAssembler;
use crate::memory::cortex::CortexStore;
use crate::runtime_pool::RuntimeResourcePool;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReloadDiagnostic {
    pub phase: String,
    pub severity: String,
    pub code: String,
    pub message: String,
}

impl ReloadDiagnostic {
    fn info(phase: &str, code: &str, message: impl Into<String>) -> Self {
        Self {
            phase: phase.into(),
            severity: "info".into(),
            code: code.into(),
            message: message.into(),
        }
    }

    fn warn(phase: &str, code: &str, message: impl Into<String>) -> Self {
        Self {
            phase: phase.into(),
            severity: "warning".into(),
            code: code.into(),
            message: message.into(),
        }
    }

    fn error(phase: &str, code: &str, message: impl Into<String>) -> Self {
        Self {
            phase: phase.into(),
            severity: "error".into(),
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReloadMigration {
    pub keybinds_written: bool,
    pub providers_written: bool,
    pub main_rewritten: bool,
}

impl From<MigrationReport> for ReloadMigration {
    fn from(value: MigrationReport) -> Self {
        Self {
            keybinds_written: value.keybinds_written,
            providers_written: value.providers_written,
            main_rewritten: value.main_rewritten,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReloadReport {
    pub status: String,
    pub config_dir: String,
    pub reloads_applied: u64,
    pub sessions_rebuilt: usize,
    pub restart_required: Vec<String>,
    pub diagnostics: Vec<ReloadDiagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub migration: Option<ReloadMigration>,
}

impl ReloadReport {
    fn rejected(config_dir: &Path, diagnostic: ReloadDiagnostic, reloads_applied: u64) -> Self {
        Self {
            status: "rejected".into(),
            config_dir: config_dir.display().to_string(),
            reloads_applied,
            sessions_rebuilt: 0,
            restart_required: Vec::new(),
            diagnostics: vec![diagnostic],
            migration: None,
        }
    }
}

const HOT_RELOADABLE_AREAS: &[&str] = &[
    "provider",
    "providers",
    "tools",
    "mcp_servers",
    "hooks",
    "skills_dir",
    "auto_create_skills",
    "compaction",
    "embeddings",
    "tui",
    "keybinds",
    "recall",
    "code_outline_assist",
    "workspace_trust",
    "active_profile",
    "knowledge",
];

const RESTART_REQUIRED_MAIN_TABLES: &[&str] = &[
    "cortex",
    "gateway",
    "scheduler",
    "extensions",
    "daemon",
    "observability",
];

#[derive(Clone, Default, PartialEq, Eq)]
struct ConfigFileSnapshot {
    main_raw: Option<String>,
    keybinds_raw: Option<String>,
    providers_raw: Option<String>,
}

impl ConfigFileSnapshot {
    fn read_from_dir(dir: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            main_raw: read_optional(dir.join("config.toml"))?,
            keybinds_raw: read_optional(dir.join("keybinds.toml"))?,
            providers_raw: read_optional(dir.join("providers.toml"))?,
        })
    }

    fn migration_advisories(&self) -> Vec<ReloadDiagnostic> {
        let mut out = Vec::new();
        if let Some(raw) = &self.main_raw {
            if raw.contains("[keybinds]") && self.keybinds_raw.is_none() {
                out.push(ReloadDiagnostic::warn(
                    "validate",
                    "LEGACY_KEYBINDS_INLINE",
                    "config.toml still contains [keybinds]; run `vulcan migrate-config` to split keybinds.toml",
                ));
            }
            if raw.contains("[providers.") && self.providers_raw.is_none() {
                out.push(ReloadDiagnostic::warn(
                    "validate",
                    "LEGACY_PROVIDERS_INLINE",
                    "config.toml still contains [providers.<name>] blocks; run `vulcan migrate-config` to split providers.toml",
                ));
            }
        }
        out
    }

    fn main_table(&self) -> Option<toml::Value> {
        self.main_raw
            .as_deref()
            .and_then(|raw| toml::from_str::<toml::Value>(raw).ok())
    }
}

fn read_optional(path: PathBuf) -> anyhow::Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(
        std::fs::read_to_string(&path).map_err(anyhow::Error::from)?,
    ))
}

fn restart_required_sections(
    current: &ConfigFileSnapshot,
    candidate: &ConfigFileSnapshot,
) -> Vec<String> {
    let current = current
        .main_table()
        .unwrap_or_else(|| toml::Value::Table(Default::default()));
    let candidate = candidate
        .main_table()
        .unwrap_or_else(|| toml::Value::Table(Default::default()));

    RESTART_REQUIRED_MAIN_TABLES
        .iter()
        .copied()
        .filter(|section| current.get(*section) != candidate.get(*section))
        .map(str::to_string)
        .collect()
}

fn apply_hot_reloadable_fields(current: &Config, candidate: &Config) -> Config {
    let mut merged = current.clone();
    merged.provider = candidate.provider.clone();
    merged.providers = candidate.providers.clone();
    merged.tools = candidate.tools.clone();
    merged.mcp_servers = candidate.mcp_servers.clone();
    merged.hooks = candidate.hooks.clone();
    merged.skills_dir = candidate.skills_dir.clone();
    merged.auto_create_skills = candidate.auto_create_skills;
    merged.compaction = candidate.compaction.clone();
    merged.embeddings = candidate.embeddings.clone();
    merged.tui = candidate.tui.clone();
    merged.keybinds = candidate.keybinds.clone();
    merged.recall = candidate.recall.clone();
    merged.code_outline_assist = candidate.code_outline_assist.clone();
    merged.workspace_trust = candidate.workspace_trust.clone();
    merged.active_profile = candidate.active_profile.clone();
    merged.knowledge = candidate.knowledge.clone();
    merged
}

fn validate_config(config: &Config) -> anyhow::Result<Vec<ReloadDiagnostic>> {
    let mut diagnostics = Vec::new();
    for server in &config.mcp_servers {
        server
            .validate()
            .map_err(|err| anyhow::anyhow!("mcp_servers `{}`: {err}", server.name))?;
    }
    for hook in &config.hooks {
        hook.validate()
            .map_err(|err| anyhow::anyhow!("hook `{}`: {err}", hook.id))?;
    }
    if let Some(gateway) = &config.gateway {
        gateway.validate()?;
    }
    config.scheduler.validate()?;
    if config.daemon.session_idle_ttl_secs == 0 {
        anyhow::bail!("[daemon] session_idle_ttl_secs must be > 0");
    }
    if config.daemon.eviction_sweep_interval_secs == 0 {
        anyhow::bail!("[daemon] eviction_sweep_interval_secs must be > 0");
    }
    if let Some(name) = config.active_profile.as_deref()
        && !config.providers.contains_key(name)
    {
        diagnostics.push(ReloadDiagnostic::warn(
            "validate",
            "ACTIVE_PROFILE_MISSING",
            format!(
                "active_profile = `{name}` not found in [providers]; runtime will fall back to [provider]"
            ),
        ));
    }
    Ok(diagnostics)
}

/// Per-process daemon state, shared across all connections.
pub struct DaemonState {
    started_at: Instant,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    sessions: Arc<SessionMap>,
    reloads_applied: AtomicU64,
    cortex: Option<Arc<CortexStore>>,
    cortex_error: Option<String>,
    /// Slice 3: daemon-owned shared adapters (session store, run
    /// store, artifact store, orchestration). `Option` so existing
    /// minimal-test constructors don't pay the SQLite-open cost; the
    /// production `with_pool` builder installs a real pool.
    pool: Option<Arc<RuntimeResourcePool>>,
    /// Current runtime config snapshot used by lazy session assembly.
    config: RwLock<Arc<Config>>,
    /// Directory that produced the currently-loaded config snapshot.
    config_dir: RwLock<PathBuf>,
    /// Last loaded on-disk fragments so reload can detect which
    /// sections changed and distinguish hot-reloadable vs restart-
    /// required edits.
    config_files: RwLock<ConfigFileSnapshot>,
    /// Most recent reload attempt summary for operator status.
    last_reload_report: RwLock<Option<ReloadReport>>,
}

impl DaemonState {
    pub fn new(config: Arc<Config>) -> Self {
        Self::new_for_dir(config, vulcan_home())
    }

    fn new_for_dir(config: Arc<Config>, config_dir: PathBuf) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let config_files = ConfigFileSnapshot::read_from_dir(&config_dir).unwrap_or_default();
        Self {
            started_at: Instant::now(),
            shutdown_tx,
            shutdown_rx,
            sessions: Arc::new(SessionMap::with_main()),
            reloads_applied: AtomicU64::new(0),
            cortex: None,
            cortex_error: None,
            pool: None,
            config: RwLock::new(config),
            config_dir: RwLock::new(config_dir),
            config_files: RwLock::new(config_files),
            last_reload_report: RwLock::new(None),
        }
    }

    /// Slice 3: install the daemon-owned [`RuntimeResourcePool`].
    /// Called by the daemon startup path after opening the pool.
    pub fn with_pool(mut self, pool: Arc<RuntimeResourcePool>) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Borrow the daemon-owned runtime resource pool, if installed.
    pub fn pool(&self) -> Option<&Arc<RuntimeResourcePool>> {
        self.pool.as_ref()
    }

    /// Build the Session-facing Agent assembler for lazy session
    /// installs. Callers pass only session-specific options to
    /// `SessionState`; config and pool wiring stay centralized here.
    pub fn session_agent_assembler(&self) -> SessionAgentAssembler {
        SessionAgentAssembler::new(self.config(), self.pool.clone())
    }

    /// Test-only constructor. Returns a `DaemonState` with the default
    /// `"main"` session pre-created and no Agent/Cortex installed —
    /// matching the post-boot, pre-warm-build state. Tests that need a
    /// minimal but realistic daemon state should use this to keep
    /// session-handler / dispatch tests independent from the boot path.
    /// The carried Config is `Config::default()` — sufficient for
    /// failure-path tests but won't produce a working Agent build.
    #[doc(hidden)]
    pub fn for_tests_minimal() -> Self {
        Self::new(Arc::new(Config::default()))
    }

    #[cfg(test)]
    pub(crate) fn for_tests_with_home(config: Arc<Config>, dir: &Path) -> Self {
        Self::new_for_dir(config, dir.to_path_buf())
    }

    /// Initialize with an opened CortexStore. Called by the daemon startup
    /// path after loading config.
    pub fn with_cortex(mut self, store: Arc<CortexStore>) -> Self {
        self.cortex = Some(store);
        self.cortex_error = None;
        self
    }

    /// Record why cortex is unavailable even though config requested it.
    pub fn with_cortex_error(mut self, error: String) -> Self {
        self.cortex_error = Some(error);
        self
    }

    /// Borrow the cortex store, if enabled.
    pub fn cortex(&self) -> Option<&Arc<CortexStore>> {
        self.cortex.as_ref()
    }

    /// Borrow the startup/open failure for cortex, if any.
    pub fn cortex_error(&self) -> Option<&str> {
        self.cortex_error.as_deref()
    }

    /// Clone the daemon's loaded `Config`.
    pub fn config(&self) -> Arc<Config> {
        Arc::clone(&self.config.read())
    }

    /// Count of successful config reloads applied since startup.
    pub fn reloads_applied(&self) -> u64 {
        self.reloads_applied.load(Ordering::SeqCst)
    }

    pub fn last_reload_report(&self) -> Option<ReloadReport> {
        self.last_reload_report.read().clone()
    }

    pub async fn reload_from_disk(&self) -> ReloadReport {
        self.reload_from_dir(&vulcan_home()).await
    }

    pub async fn reload_from_dir(&self, dir: &Path) -> ReloadReport {
        let candidate_files = match ConfigFileSnapshot::read_from_dir(dir) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                let report = ReloadReport::rejected(
                    dir,
                    ReloadDiagnostic::error(
                        "parse",
                        "CONFIG_READ_FAILED",
                        format!("failed to read config fragments: {err:#}"),
                    ),
                    self.reloads_applied(),
                );
                *self.last_reload_report.write() = Some(report.clone());
                return report;
            }
        };

        let candidate = match Config::load_from_dir(dir) {
            Ok(config) => config,
            Err(err) => {
                let report = ReloadReport::rejected(
                    dir,
                    ReloadDiagnostic::error(
                        "parse",
                        "CONFIG_PARSE_FAILED",
                        format!("failed to parse config fragments: {err:#}"),
                    ),
                    self.reloads_applied(),
                );
                *self.last_reload_report.write() = Some(report.clone());
                return report;
            }
        };

        let mut diagnostics = candidate_files.migration_advisories();
        match validate_config(&candidate) {
            Ok(mut validation_diags) => diagnostics.append(&mut validation_diags),
            Err(err) => {
                let mut report = ReloadReport::rejected(
                    dir,
                    ReloadDiagnostic::error(
                        "validate",
                        "CONFIG_VALIDATION_FAILED",
                        format!("config validation failed: {err:#}"),
                    ),
                    self.reloads_applied(),
                );
                report.diagnostics.splice(0..0, diagnostics);
                *self.last_reload_report.write() = Some(report.clone());
                return report;
            }
        }

        let current_files = self.config_files.read().clone();
        if current_files == candidate_files {
            diagnostics.push(ReloadDiagnostic::info(
                "apply",
                "CONFIG_UNCHANGED",
                "reload skipped because config fragments are unchanged from the last applied snapshot",
            ));
            let report = ReloadReport {
                status: "skipped".into(),
                config_dir: dir.display().to_string(),
                reloads_applied: self.reloads_applied(),
                sessions_rebuilt: 0,
                restart_required: Vec::new(),
                diagnostics,
                migration: None,
            };
            *self.last_reload_report.write() = Some(report.clone());
            return report;
        }

        let current_config = self.config();
        let restart_required = restart_required_sections(&current_files, &candidate_files);
        let applied_config = apply_hot_reloadable_fields(&current_config, &candidate);
        let applied_config = Arc::new(applied_config);
        let assembler = SessionAgentAssembler::new(Arc::clone(&applied_config), self.pool.clone());

        if let Err(err) = assembler
            .assemble(crate::daemon::session_agent::SessionAgentOptions::default())
            .await
        {
            let mut report = ReloadReport::rejected(
                dir,
                ReloadDiagnostic::error(
                    "apply",
                    "AGENT_REBUILD_PREFLIGHT_FAILED",
                    format!("candidate config could not build an agent: {err:#}"),
                ),
                self.reloads_applied(),
            );
            report.restart_required = restart_required;
            report.diagnostics.splice(0..0, diagnostics);
            *self.last_reload_report.write() = Some(report.clone());
            return report;
        }

        *self.config.write() = Arc::clone(&applied_config);
        *self.config_dir.write() = dir.to_path_buf();
        *self.config_files.write() = candidate_files;

        let mut rebuilt = 0usize;
        let mut rebuild_failed = Vec::new();
        for session in self.sessions.all() {
            if !session.has_agent() {
                continue;
            }
            match session.rebuild_agent(&assembler).await {
                Ok(()) => rebuilt += 1,
                Err(err) => {
                    rebuild_failed.push(session.id.clone());
                    diagnostics.push(ReloadDiagnostic::warn(
                        "apply",
                        "SESSION_REBUILD_FAILED",
                        format!(
                            "session `{}` kept running with its prior warm agent: {err:#}",
                            session.id
                        ),
                    ));
                }
            }
        }

        let reloads_applied = self.reloads_applied.fetch_add(1, Ordering::SeqCst) + 1;
        diagnostics.push(ReloadDiagnostic::info(
            "apply",
            "HOT_RELOADABLE_FIELDS_APPLIED",
            format!(
                "hot-reloadable areas applied: {}",
                HOT_RELOADABLE_AREAS.join(", ")
            ),
        ));
        if !restart_required.is_empty() {
            diagnostics.push(ReloadDiagnostic::warn(
                "apply",
                "RESTART_REQUIRED_FIELDS_CHANGED",
                format!(
                    "restart-required config changed and was left pending until daemon restart: {}",
                    restart_required.join(", ")
                ),
            ));
        }
        diagnostics.push(ReloadDiagnostic::info(
            "apply",
            if rebuild_failed.is_empty() {
                "CONFIG_APPLIED"
            } else {
                "CONFIG_APPLIED_DEGRADED"
            },
            if rebuild_failed.is_empty() && restart_required.is_empty() {
                format!("applied hot-reloadable config and rebuilt {rebuilt} warm session(s)")
            } else if rebuild_failed.is_empty() {
                format!(
                    "applied hot-reloadable config and rebuilt {rebuilt} warm session(s); restart required for: {}",
                    restart_required.join(", ")
                )
            } else if restart_required.is_empty() {
                format!(
                    "applied hot-reloadable config and rebuilt {rebuilt} warm session(s); {} session(s) kept their prior warm agent: {}",
                    rebuild_failed.len(),
                    rebuild_failed.join(", ")
                )
            } else {
                format!(
                    "applied hot-reloadable config and rebuilt {rebuilt} warm session(s); {} session(s) kept their prior warm agent: {}; restart required for: {}",
                    rebuild_failed.len(),
                    rebuild_failed.join(", "),
                    restart_required.join(", ")
                )
            },
        ));
        let report = ReloadReport {
            status: if !rebuild_failed.is_empty() {
                "degraded".into()
            } else if restart_required.is_empty() {
                "applied".into()
            } else {
                "applied_with_restart_required".into()
            },
            config_dir: dir.display().to_string(),
            reloads_applied,
            sessions_rebuilt: rebuilt,
            restart_required,
            diagnostics,
            migration: None,
        };
        *self.last_reload_report.write() = Some(report.clone());
        report
    }

    /// Borrow the session map. Used by handlers that need to look
    /// up or mutate per-session state.
    pub fn sessions(&self) -> &SessionMap {
        &self.sessions
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Signal shutdown. Idempotent and latching — once called, every
    /// existing AND future call to [`Self::shutdown_signal`] observes
    /// the latched `true` value via `borrow()`, and any receiver
    /// acquired *before* the signal will resolve `changed().await`
    /// immediately. No registration ordering required.
    pub fn signal_shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Acquire a watch receiver. Await `recv.changed().await` (or check
    /// `*recv.borrow()`) to observe shutdown.
    pub fn shutdown_signal(&self) -> watch::Receiver<bool> {
        self.shutdown_rx.clone()
    }

    /// Returns one descriptor per live session — id, in_flight,
    /// last_activity_secs_ago. Replaces the Slice 0 Task 0.6 stub.
    pub fn session_descriptors(&self) -> Vec<serde_json::Value> {
        self.sessions.descriptors()
    }
}

impl Default for DaemonState {
    fn default() -> Self {
        Self::new(Arc::new(Config::default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pool_is_none_by_default_and_some_after_with_pool() {
        let state = DaemonState::for_tests_minimal();
        assert!(state.pool().is_none());

        let pool = Arc::new(RuntimeResourcePool::for_tests().await);
        let state = state.with_pool(Arc::clone(&pool));
        let installed = state.pool().expect("pool installed");
        assert!(Arc::ptr_eq(installed, &pool));
    }

    #[tokio::test]
    async fn reload_rejects_parse_errors_with_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[provider\nmodel = \"x\"").unwrap();
        let state = DaemonState::for_tests_minimal();

        let report = state.reload_from_dir(dir.path()).await;
        assert_eq!(report.status, "rejected");
        assert_eq!(report.reloads_applied, 0);
        assert_eq!(report.diagnostics[0].phase, "parse");
        assert_eq!(report.diagnostics[0].code, "CONFIG_PARSE_FAILED");
    }

    #[tokio::test]
    async fn reload_reports_restart_required_sections() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"
[provider]
base_url = "http://127.0.0.1:11434/v1"
model = "qwen2.5:7b"
disable_catalog = true
"#,
        )
        .unwrap();
        let mut baseline = Config::default();
        baseline.provider.base_url = "http://127.0.0.1:11434/v1".into();
        baseline.provider.model = "qwen2.5:7b".into();
        baseline.provider.disable_catalog = true;
        let state = DaemonState::for_tests_with_home(Arc::new(baseline), dir.path());

        std::fs::write(
            dir.path().join("config.toml"),
            r#"
[provider]
base_url = "http://127.0.0.1:11434/v1"
model = "qwen2.5:7b"
disable_catalog = true

[gateway]
api_token = "[REDACTED]"
"#,
        )
        .unwrap();

        let report = state.reload_from_dir(dir.path()).await;
        assert_eq!(report.status, "applied_with_restart_required");
        assert!(report.restart_required.contains(&"gateway".to_string()));
    }

    #[tokio::test]
    async fn reload_skips_when_config_fragments_are_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"
[provider]
base_url = "http://127.0.0.1:11434/v1"
model = "qwen2.5:7b"
disable_catalog = true
"#,
        )
        .unwrap();
        let mut baseline = Config::default();
        baseline.provider.base_url = "http://127.0.0.1:11434/v1".into();
        baseline.provider.model = "qwen2.5:7b".into();
        baseline.provider.disable_catalog = true;
        let state = DaemonState::for_tests_with_home(Arc::new(baseline), dir.path());

        let report = state.reload_from_dir(dir.path()).await;
        assert_eq!(report.status, "skipped");
        assert_eq!(report.reloads_applied, 0);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diag| diag.code == "CONFIG_UNCHANGED")
        );
    }
}
