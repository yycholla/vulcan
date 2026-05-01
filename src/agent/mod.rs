use std::sync::Arc;

use crate::config::{CompactionConfig, Config, ProviderConfig};
use crate::context::ContextManager;
use crate::hooks::HookRegistry;
use crate::hooks::approval::ApprovalHook;
use crate::hooks::diagnostics::DiagnosticsHook;
use crate::hooks::recall::RecallHook;
use crate::hooks::safety::SafetyHook;
use crate::hooks::skills::SkillsHook;
use crate::memory::SessionStore;
use crate::pause::PauseSender;
use crate::prompt_builder::PromptBuilder;
use crate::provider::factory::{DefaultProviderFactory, ProviderFactory};
use crate::provider::{LLMProvider, Message, ToolDefinition};
use crate::runtime_pool::RuntimeResourcePool;
use crate::skills::SkillRegistry;
use crate::tools::{ToolRegistry, ToolResult};
use anyhow::Result;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

mod dispatch;
mod provider;
mod run;
mod session;
mod skills;
mod turn;

#[cfg(test)]
mod tests;

/// Heuristic for "local" provider endpoints (loopback, link-local, mDNS
/// `.local`, RFC1918 private IPv4, IPv6 ULA, IPv4-mapped IPv6). Used
/// to skip the API key requirement when switching to or starting up
/// against a self-hosted endpoint that typically doesn't need auth.
///
/// YYC-152: parses through `url::Url` so IPv6 brackets (`[::1]:11434`),
/// percent-encoded hosts, and IPv4-mapped IPv6 (`::ffff:192.168.1.1`)
/// are handled by a real URL parser instead of hand-rolled split/strip.
/// Bare host:port input (no scheme) is normalized with `http://`
/// before parsing so the heuristic still works on user-pasted endpoints.
pub(in crate::agent) fn is_local_base_url(base_url: &str) -> bool {
    let normalized = if base_url.contains("://") {
        base_url.to_string()
    } else {
        format!("http://{base_url}")
    };
    let Ok(url) = url::Url::parse(&normalized) else {
        return false;
    };
    match url.host() {
        Some(url::Host::Domain(d)) => {
            let dl = d.to_ascii_lowercase();
            dl == "localhost" || dl.ends_with(".local")
        }
        Some(url::Host::Ipv4(ip)) => is_local_ipv4(ip),

        Some(url::Host::Ipv6(ip)) => {
            if ip.is_loopback() || ip.is_unspecified() || ip.is_unique_local() {
                return true;
            }
            // YYC-152: fe80::/10 unicast link-local. `Ipv6Addr::is_unicast_link_local`
            // is unstable, so check the prefix bits manually.
            if (ip.segments()[0] & 0xffc0) == 0xfe80 {
                return true;
            }
            if let Some(v4) = ip.to_ipv4_mapped() {
                return is_local_ipv4(v4);
            }
            false
        }

        None => false,
    }
}

fn is_local_ipv4(ip: std::net::Ipv4Addr) -> bool {
    ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()
}

/// The core agent — orchestrates the LLM, tools, hooks, and state.
///
/// One Agent per session. Hold it across turns: the hook registry's stateful
/// handlers (audit log, rate limits, approval caches) only work if the Agent
/// outlives a single prompt.
pub struct Agent {
    pub(in crate::agent) provider: Box<dyn LLMProvider>,
    pub(in crate::agent) provider_factory: Arc<dyn ProviderFactory>,
    pub(in crate::agent) tools: ToolRegistry,
    pub(in crate::agent) skills: Arc<SkillRegistry>,
    pub(in crate::agent) context: ContextManager,
    pub(in crate::agent) memory: Arc<SessionStore>,
    pub(in crate::agent) prompt_builder: PromptBuilder,
    pub(in crate::agent) hooks: Arc<HookRegistry>,
    pub(in crate::agent) session_id: String,
    pub(in crate::agent) turns: u32,
    /// Per-turn cancellation token. `cancel_current_turn()` fires it; the
    /// next call to `run_prompt` / `run_prompt_stream` swaps in a fresh token
    /// so cancel applies to the in-flight turn only, not future ones.
    pub(in crate::agent) turn_cancel: CancellationToken,
    /// Latest file edit, captured by `WriteFile`/`PatchFile` (YYC-66).
    /// TUI clones this Arc and renders the inner Option each frame.
    pub(in crate::agent) diff_sink: crate::tools::EditDiffSink,
    /// Per-token pricing for the active model, sourced from the provider
    /// catalog at startup (YYC-67). `None` when the catalog is disabled
    /// or the provider doesn't publish pricing.
    pub(in crate::agent) pricing: Option<crate::provider::catalog::Pricing>,
    pub(in crate::agent) compaction_config: CompactionConfig,
    /// Active provider profile and resolved auth. Kept so user-facing
    /// commands can switch models without reconstructing the long-lived
    /// Agent, hook registry, tools, memory, or session state.
    pub(in crate::agent) provider_config: ProviderConfig,
    /// YYC-249: provider API key wrapped in `SecretString` so the
    /// underlying buffer is zeroed on drop. Default `Debug` impl
    /// redacts the value, so accidental log lines don't print it.
    /// Call sites that need the raw `&str` use `.expose_secret()`
    /// at the moment of use rather than caching a copy.
    pub(in crate::agent) provider_api_key: secrecy::SecretString,
    /// Name of the active named provider profile from `[providers.<name>]`,
    /// or `None` when running on the unnamed legacy `[provider]` block.
    /// Set by `switch_provider`; surfaced via `active_profile()` so the
    /// TUI can label which profile a turn will hit (YYC-94).
    pub(in crate::agent) active_profile: Option<String>,
    /// LSP server pool (YYC-46). Lazy: servers spawn on first tool
    /// invocation that needs one. Reaped in `end_session`.
    pub(in crate::agent) lsp_manager: Arc<crate::code::lsp::LspManager>,
    /// Number of messages in `run_prompt[_stream]`'s `messages` Vec that have
    /// already been persisted to SQLite. Used to skip the O(n) DELETE + re-INSERT
    /// on every turn — only `messages[last_saved_count..]` are new (YYC-76).
    pub(in crate::agent) last_saved_count: usize,
    /// Slice 2: canonical in-memory transcript for this live session,
    /// excluding the leading `System` frame (which the prompt builder
    /// rebuilds fresh each turn). Loaded once on the first
    /// `prepare_turn` call; subsequent turns read from this snapshot
    /// instead of re-running `SessionStore::load_history`. Mirrored
    /// to durable storage by `save_messages` / `replace_history`.
    pub(in crate::agent) history_cache: Vec<Message>,
    /// Slice 2: `true` once the cache has been populated from storage
    /// for this session (or initialized from a fresh resume). Guards
    /// the load + sanitize + heal once-on-create path.
    pub(in crate::agent) history_loaded: bool,
    /// Max agent loop iterations per prompt. 0 = unlimited (default).
    pub(in crate::agent) max_iterations: u32,
    /// Workspace context probed at session start (YYC-107). Used to
    /// filter the tool registry and feed dynamic tool descriptions.
    pub(in crate::agent) tool_context: crate::tools::ToolContext,
    /// YYC-20: when true, after a 5+ iteration turn the agent asks
    /// the active provider to summarize the turn as a draft skill
    /// and writes it under `<skills_dir>/_pending/`. Off by default
    /// (`config.auto_create_skills`).
    pub(in crate::agent) auto_create_skills: bool,
    /// YYC-206: shared orchestration store for child agent runs.
    /// `SpawnSubagentTool` writes to this so the TUI can render
    /// a real subagent timeline (YYC-205).
    pub(in crate::agent) orchestration: Arc<crate::orchestration::OrchestrationStore>,
    /// YYC-211: cumulative `total_tokens` across every provider
    /// response in the agent's lifetime. Distinct from
    /// `ContextManager::current_tokens`, which tracks the last
    /// prompt's size for compaction decisions. This counter only
    /// grows; readers diff snapshots if they want per-run usage
    /// (e.g. spawn_subagent computes `after - before`).
    pub(in crate::agent) tokens_consumed: u64,
    /// YYC-179: durable run-record store. Every `run_prompt` call
    /// creates a `RunRecord` here with a stable `RunId`, accumulates
    /// lifecycle/provider events as the turn unfolds, and finalizes
    /// on completion/error/cancellation.
    pub(in crate::agent) run_store: Arc<dyn crate::run_record::RunStore>,
    /// YYC-179: id of the run currently in flight, if any. Set by
    /// `run_prompt_inner` on entry, cleared on exit. Other parts of
    /// the agent (hooks, tools) can read it to attach events.
    ///
    /// Slice 7: wrapped in `Arc<Mutex<...>>` so external observers
    /// (notably `SpawnSubagentTool`) can read live state without
    /// holding the agent mutex — needed to stamp child runs with the
    /// parent's `RunId` for `RunOrigin::Subagent` lineage.
    pub(in crate::agent) current_run_id: Arc<parking_lot::Mutex<Option<crate::run_record::RunId>>>,
    /// YYC-180: durable artifact store. Tools, hooks, and the
    /// agent itself create typed artifacts (plans, diffs, reports,
    /// subagent summaries) here; an `ArtifactCreated` run-record
    /// event references them by id.
    pub(in crate::agent) artifact_store: Arc<dyn crate::artifact::ArtifactStore>,
    /// YYC-182: trust profile resolved for the active workspace
    /// at session start. Drives the default capability profile and
    /// downstream persistence/indexing choices.
    pub(in crate::agent) trust_profile: crate::trust::TrustProfile,
}

#[derive(Debug, Clone)]
pub struct ModelSelection {
    pub model: crate::provider::catalog::ModelInfo,
    pub max_context: usize,
    pub pricing: Option<crate::provider::catalog::Pricing>,
}

pub(in crate::agent) struct StreamTurn {
    pub(in crate::agent) messages: Vec<Message>,
    pub(in crate::agent) tool_defs: Vec<ToolDefinition>,
}

pub struct AgentBuilder<'a> {
    config: &'a Config,
    hooks: HookRegistry,
    pause_tx: Option<PauseSender>,
    max_iterations: Option<u32>,
    /// YYC-181: tool capability profile name to apply at session
    /// start. CLI flag overrides config; `None` means use whatever
    /// the config supplies (or no profile if unset).
    tool_profile: Option<String>,
    /// Slice 3: optional daemon-owned [`RuntimeResourcePool`].
    /// When present, session/run/artifact/orchestration stores come
    /// from the pool instead of being opened fresh per Agent. The
    /// non-daemon CLI/test paths still pass `None` and fall back to
    /// the legacy build-everything path.
    pool: Option<Arc<RuntimeResourcePool>>,
    frontend_capabilities: Vec<crate::extensions::FrontendCapability>,
}

impl<'a> AgentBuilder<'a> {
    pub fn with_hooks(mut self, hooks: HookRegistry) -> Self {
        self.hooks = hooks;
        self
    }

    pub fn with_pause_channel(mut self, pause_tx: PauseSender) -> Self {
        self.pause_tx = Some(pause_tx);
        self
    }

    pub fn with_max_iterations(mut self, max_iterations: u32) -> Self {
        self.max_iterations = Some(max_iterations);
        self
    }

    /// YYC-181: apply a named tool capability profile at session
    /// start. Pass `None` to fall back to `tools.profile` from
    /// config; pass `Some(name)` to override.
    pub fn with_tool_profile(mut self, profile: Option<String>) -> Self {
        self.tool_profile = profile;
        self
    }

    /// Slice 3: assemble the Agent from the daemon's shared
    /// [`RuntimeResourcePool`] instead of opening per-Agent SQLite
    /// connections, run/artifact stores, and orchestration store.
    pub fn with_pool(mut self, pool: Arc<RuntimeResourcePool>) -> Self {
        self.pool = Some(pool);
        self
    }

    pub fn with_frontend_capabilities(
        mut self,
        frontend_capabilities: Vec<crate::extensions::FrontendCapability>,
    ) -> Self {
        self.frontend_capabilities = frontend_capabilities;
        self
    }

    pub async fn build(self) -> Result<Agent> {
        Agent::build_from_parts(
            self.config,
            self.hooks,
            self.pause_tx,
            self.max_iterations,
            self.tool_profile,
            self.pool,
            self.frontend_capabilities,
        )
        .await
    }
}

impl Agent {
    pub fn builder(config: &Config) -> AgentBuilder<'_> {
        AgentBuilder {
            config,
            hooks: HookRegistry::new(),
            pause_tx: None,
            max_iterations: None,
            tool_profile: None,
            pool: None,
            frontend_capabilities: crate::extensions::FrontendCapability::full_set(),
        }
    }

    /// Build an Agent from a fully configured `AgentBuilder`.
    ///
    /// Async because it fetches the provider's model catalog at startup
    /// (YYC-64). Catalog-fetch failures are non-fatal — logged and continued
    /// with config defaults.
    ///
    /// Returns `Err` for fatal init failures (missing API key, provider build
    /// failure, or model not found in catalog).
    async fn build_from_parts(
        config: &Config,
        mut hooks: HookRegistry,
        pause_tx: Option<PauseSender>,
        max_iterations: Option<u32>,
        tool_profile_override: Option<String>,
        pool: Option<Arc<RuntimeResourcePool>>,
        frontend_capabilities: Vec<crate::extensions::FrontendCapability>,
    ) -> Result<Self> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let memory: Arc<SessionStore> = match &pool {
            Some(p) => p.session_store(),
            None => Arc::new(SessionStore::try_new()?),
        };
        let session_id = Uuid::new_v4().to_string();

        let extension_provider_catalog = pool.as_ref().map(|p| p.extension_provider_catalog());
        if let Some(p) = pool.as_ref() {
            let ctx = crate::extensions::api::SessionExtensionCtx {
                cwd: cwd.clone(),
                session_id: session_id.clone(),
                memory: Arc::clone(&memory),
                frontend_capabilities: frontend_capabilities.clone(),
                state: crate::extensions::ExtensionStateContext::new(
                    p.extension_state_store(),
                    session_id.clone(),
                    "__pending__",
                    Vec::new(),
                ),
            };
            let registered = p
                .extension_registry()
                .wire_daemon_extension_providers(ctx, &p.extension_provider_catalog());
            if registered > 0 {
                tracing::info!(
                    extension_providers = registered,
                    "Agent: registered daemon-side extension providers"
                );
            }
        }

        // YYC-239: TUI + gateway resolve their starting provider
        // through the same `active_provider_config` indirection.
        // When `active_profile` is set + present in `[providers]`,
        // that wins; otherwise the legacy `[provider]` block.
        let active_provider = config.active_provider_config();

        // Local / self-hosted endpoints don't require auth; allow empty
        // string in that case (matches `switch_provider` semantics).
        let provider_is_extension = extension_provider_catalog
            .as_ref()
            .is_some_and(|catalog| catalog.contains(&active_provider.r#type));
        let api_key = match config.api_key() {
            Some(k) => k,
            None if provider_is_extension => String::new(),
            None if active_provider.disable_catalog
                || is_local_base_url(&active_provider.base_url) =>
            {
                String::new()
            }
            None => {
                anyhow::bail!(
                    "No API key configured. Set VULCAN_API_KEY or add api_key to ~/.vulcan/config.toml"
                );
            }
        };

        // ── Catalog: validate the configured model and (optionally) override
        // max_context with whatever the catalog says it actually is. Non-fatal:
        // if the catalog endpoint fails, we log + continue with the configured
        // values rather than blocking startup over a metadata fetch.
        let selection = if provider_is_extension {
            ModelSelection {
                model: crate::provider::catalog::ModelInfo {
                    id: active_provider.model.clone(),
                    display_name: active_provider.model.clone(),
                    context_length: active_provider.max_context,
                    pricing: None,
                    features: crate::provider::catalog::ModelFeatures {
                        tools: true,
                        vision: false,
                        json_mode: false,
                        reasoning: false,
                    },
                    top_provider: Some(active_provider.r#type.clone()),
                },
                max_context: active_provider.max_context,
                pricing: None,
            }
        } else {
            Self::resolve_model_selection(active_provider, &api_key).await?
        };
        let effective_max_context = selection.max_context;
        let supports_json_mode = selection.model.features.json_mode;
        let pricing = selection.pricing.clone();

        let provider_factory: Arc<dyn ProviderFactory> = match extension_provider_catalog {
            Some(catalog) => {
                Arc::new(crate::provider::factory::ExtensionAwareProviderFactory::new(catalog))
            }
            None => Arc::new(DefaultProviderFactory),
        };
        let provider = provider_factory.build(
            active_provider,
            &api_key,
            effective_max_context,
            supports_json_mode,
        )?;

        let diff_sink = crate::tools::new_diff_sink();
        // Slice 3: shared LSP manager from the pool when available so
        // language servers stay warm across sessions instead of
        // spawning per Agent.
        let lsp_manager = match &pool {
            Some(p) => p.lsp_manager(),
            None => Arc::new(crate::code::lsp::LspManager::new(cwd.clone())),
        };
        // YYC-107: probe the workspace once so tool registration can
        // drop irrelevant tools (cargo_check off-Rust, etc.) and the
        // remaining tools can render runtime-aware descriptions.
        let tool_context = crate::tools::ToolContext::probe(cwd.clone());
        let mut tools = ToolRegistry::new_with_diff_and_lsp(
            Some(diff_sink.clone()),
            Some(lsp_manager.clone()),
            cwd.clone(),
        );

        // YYC-81: ask_user is only useful in interactive (TUI) mode.
        // Register it whenever a pause channel is wired; it self-
        // reports when called without one.
        if pause_tx.is_some() {
            tools.register(Arc::new(crate::tools::ask_user::AskUserTool::new(
                pause_tx.clone(),
            )));
            // YYC-75: re-register edit_file with the pause channel so
            // multi-site replaces route through the diff scrubber. Still
            // shares the diff sink wired up in the registry constructor.
            tools.register(Arc::new(crate::tools::file::PatchFile::with_pause(
                Some(diff_sink.clone()),
                pause_tx.clone(),
            )));
        }

        // YYC-82: spawn_subagent tool. Holds a clone of the parent
        // config so child agents can be built with the same provider
        // wiring. Default tool allowlist is read-only (see
        // `tools::spawn::default_allowed_tools`).
        // YYC-206: shares the agent's `orchestration` store so the
        // TUI / parent can read child-run records the tool produces.
        // Slice 3: when a pool is wired up, draw the orchestration
        // store from it so subagents land in the daemon-owned record
        // instead of a fresh per-Agent in-memory copy.
        let config_arc = Arc::new(config.clone());
        let orchestration = match &pool {
            Some(p) => p.orchestration(),
            None => Arc::new(crate::orchestration::OrchestrationStore::new()),
        };
        tools.register(Arc::new(
            crate::tools::spawn::SpawnSubagentTool::with_store(
                Arc::clone(&config_arc),
                Arc::clone(&orchestration),
            ),
        ));

        // YYC-48: register embedding tools when [embeddings] is
        // enabled. The index opens its own SQLite store; failure is
        // logged but non-fatal — the agent still has every other tool.
        if config.embeddings.enabled {
            let parser_cache = Arc::new(crate::code::ParserCache::new());
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            match crate::code::embed::EmbeddingIndex::open(
                cwd,
                parser_cache,
                config.embeddings.clone(),
                active_provider.base_url.clone(),
                api_key.clone().into(),
            ) {
                Ok(index) => {
                    let arc = Arc::new(index);
                    tools.register(Arc::new(
                        crate::tools::code_search::IndexEmbeddingsTool::new(arc.clone()),
                    ));
                    tools.register(Arc::new(
                        crate::tools::code_search::CodeSearchSemanticTool::new(arc),
                    ));
                }
                Err(e) => tracing::warn!("embedding index unavailable: {e}"),
            }
        }
        let skills = Arc::new(SkillRegistry::default_for(&config.skills_dir, Some(&cwd)));
        // Slice 3: shared session store comes from the pool when wired
        // up; fall back to opening a per-Agent connection for the
        // legacy direct-mode build path.
        let context =
            ContextManager::with_config(provider.max_context(), config.compaction.clone());

        // Built-in hook: surface available skills to the LLM via BeforePrompt.
        hooks.register(Arc::new(SkillsHook::new(skills.clone())));

        // YYC-42: optionally recall relevant past-session context on the
        // first turn of a fresh session. Off by default — config
        // `[recall].enabled = true` opts in. Uses its own SessionStore
        // handle (separate connection to the same SQLite file) so the
        // FTS read doesn't contend with the agent's main message-write
        // path on the existing memory mutex.
        if config.recall.enabled {
            let recall_memory = Arc::new(SessionStore::try_new()?);
            hooks.register(Arc::new(RecallHook::new(recall_memory, config.recall)));
        }

        // Built-in hook (YYC-51): auto-run LSP diagnostics after every
        // successful edit_file/write_file. No-op when LSP isn't
        // installed for the language; the user pays nothing extra.
        hooks.register(Arc::new(DiagnosticsHook::new(
            lsp_manager.clone(),
            diff_sink.clone(),
        )));

        // Built-in hook (YYC-76): per-tool approval gate. Default mode
        // is Always (no prompts) so the gate is opt-in via
        // [tools.approval]. yolo_mode=true is the legacy escape
        // hatch — it leaves the default at Always.
        let mut approval_cfg = config.tools.approval.clone();
        if config.tools.yolo_mode {
            approval_cfg.default = crate::config::ApprovalMode::Always;
        }
        let approval_hook = match pause_tx.clone() {
            Some(tx) => ApprovalHook::new(approval_cfg, Some(tx)),
            None => ApprovalHook::auto_deny(approval_cfg),
        };
        hooks.register(Arc::new(approval_hook));

        // Built-in hook: block dangerous shell invocations unless yolo_mode is on.
        // Skipped entirely (not even registered as observe-only) when yolo_mode
        // is true — keeps the no-op path zero-cost. With a pause emitter
        // wired up, blocks become interactive prompts.
        if !config.tools.yolo_mode {
            let safety = SafetyHook::with_config(pause_tx.clone(), config.tools.dangerous_commands);
            hooks.register(Arc::new(safety));
        }

        // Built-in hook (YYC-87 / YYC-84): redirect bash invocations to
        // native tools when there's a clear equivalent. Skipped entirely
        // when the knob is `Off`. Sits at priority 5 — after safety
        // (priority 0) so dangerous-bash still wins, before audit
        // (priority 1).
        if !matches!(
            config.tools.native_enforcement,
            crate::config::NativeEnforcement::Off
        ) {
            hooks.register(Arc::new(
                crate::hooks::prefer_native::PreferNativeToolsHook::new(
                    config.tools.native_enforcement,
                ),
            ));
        }

        // Built-in hook (YYC-264): when `rtk` is on PATH, transparently
        // wrap bash commands with `rtk summary --` to compress output
        // by 60-90% before it reaches the LLM. Falls through silently
        // when RTK is not installed — zero per-call overhead.
        hooks.register(Arc::new(crate::hooks::rtk::RtkHook::new()));

        // GH issue #549: cargo-crate extensions registered via
        // `inventory::submit!` and discovered into the pool's
        // `ExtensionRegistry` at daemon startup. Each `Active`
        // `DaemonCodeExtension` is instantiated per **Session** and its
        // `hook_handlers` registered into this Agent's `HookRegistry`.
        // No-op when the agent runs without a pool (CLI one-shot) or
        // when no daemon extensions are registered.
        if let Some(p) = pool.as_ref() {
            // GH issue #557: install the daemon's shared audit log on
            // the registry before extension hook handlers register so
            // any `on_input` outcome they emit lands on the same ring
            // the CLI / `vulcan extension audit` reads from.
            hooks = hooks.with_audit_log(p.extension_audit_log());
            if let Some(tx) = pause_tx.clone() {
                hooks = hooks.with_input_rewrite_pause_channel(tx);
            }
            hooks.register(Arc::new(
                crate::extensions::state::ExtensionStateReaperHook::new(p.extension_state_store()),
            ));

            let ctx = crate::extensions::api::SessionExtensionCtx {
                cwd: cwd.clone(),
                session_id: session_id.clone(),
                memory: Arc::clone(&memory),
                frontend_capabilities,
                state: crate::extensions::ExtensionStateContext::new(
                    p.extension_state_store(),
                    session_id.clone(),
                    "__pending__",
                    Vec::new(),
                ),
            };
            let (registered, extension_tools) = p
                .extension_registry()
                .wire_daemon_extensions_into_runtime(ctx, &mut hooks, Some(&mut tools));
            if registered > 0 {
                tracing::info!(
                    daemon_extensions = registered,
                    extension_tools,
                    "Agent: wired daemon-side cargo-crate extensions"
                );
            }
        }

        // YYC-264: embedded cortex-memory-core graph memory. When enabled,
        // registers two hooks:
        //   1. CortexRecallHook — BeforePrompt: semantically searches the graph
        //      on every turn using the latest user message and injects context.
        //   2. CortexCaptureHook — AfterToolCall: auto-stores notable tool
        //      outputs as fact nodes in the graph.
        // Both share the same `Arc<CortexStore>`. Non-fatal on failure.
        // Slice 3 deepening: prefer the daemon-owned cortex from the
        // pool so multi-session daemons don't deadlock on the redb
        // exclusive lock. Direct-mode callers fall back to opening
        // their own store.
        if config.cortex.enabled {
            let store_result = match pool.as_ref().and_then(|p| p.cortex_store()) {
                Some(store) => Ok(store),
                None => crate::memory::cortex::CortexStore::try_open(&config.cortex),
            };
            match store_result {
                Ok(store) => {
                    let max_results = config.cortex.max_search_results;
                    hooks.register(Arc::new(
                        crate::hooks::cortex_recall::CortexRecallHook::new(
                            store.clone(),
                            max_results,
                        ),
                    ));
                    hooks.register(Arc::new(
                        crate::hooks::cortex_capture::CortexCaptureHook::new(store),
                    ));
                }
                Err(e) => {
                    tracing::warn!("cortex memory unavailable: {e}");
                }
            }
        }

        // YYC-182: resolve the workspace trust profile from config
        // before tool-profile selection. The trust profile feeds
        // the default capability profile when neither CLI flag nor
        // `tools.profile` is set, so a sensitive workspace falls
        // back to `readonly` instead of unrestricted.
        let trust_profile = config.workspace_trust.resolve_for(&tool_context.cwd);

        // YYC-181: apply the requested tool capability profile.
        // Precedence: CLI flag > `tools.profile` in config > trust
        // profile's default. An unknown name surfaces as a startup
        // error so misconfiguration doesn't disguise itself as a
        // silently missing tool later.
        let resolved_profile_name = tool_profile_override
            .as_deref()
            .map(str::to_string)
            .or_else(|| config.tools.profile.clone())
            .or_else(|| {
                // YYC-182: only fall back to the trust profile's
                // capability profile when the workspace was
                // explicitly classified. Unknown workspaces still
                // get the unrestricted registry today — locking
                // them down by default ships in a follow-up once
                // the user-facing migration story is in place.
                if trust_profile.reason.contains("matched") {
                    Some(trust_profile.capability_profile.clone())
                } else {
                    None
                }
            });
        if let Some(name) = &resolved_profile_name {
            let profile = config.tools.resolve_profile(name).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown tool capability profile `{name}`. Built-in: readonly, coding, \
                     reviewer, gateway-safe. User-defined go under [tools.profiles.<name>]."
                )
            })?;
            tools.apply_profile(&profile);
        }

        // YYC-107: drop tools that aren't relevant to this workspace.
        tools.filter_for_context(&tool_context);

        // YYC-179: durable run-record store.
        // Slice 3: when a pool is wired up, share its handle so every
        // session writes into the daemon-owned timeline. The legacy
        // path opens its own SQLite file (or falls back to in-memory).
        let run_store: Arc<dyn crate::run_record::RunStore> = match &pool {
            Some(p) => p.run_store(),
            None => match crate::run_record::SqliteRunStore::try_new() {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    tracing::warn!("run_record store unavailable ({e}); falling back to in-memory");
                    Arc::new(crate::run_record::InMemoryRunStore::default())
                }
            },
        };

        // YYC-180: artifact store. Same pool-or-fallback shape as
        // run_store. Retention diverges from run records, so the
        // backends stay separate.
        let artifact_store: Arc<dyn crate::artifact::ArtifactStore> = match &pool {
            Some(p) => p.artifact_store(),
            None => match crate::artifact::SqliteArtifactStore::try_new() {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    tracing::warn!("artifact store unavailable ({e}); falling back to in-memory");
                    Arc::new(crate::artifact::InMemoryArtifactStore::new())
                }
            },
        };

        // YYC-180: re-register `spawn_subagent` with the parent's
        // artifact store so child summaries land alongside the
        // parent's run. The earlier registration sat without an
        // artifact handle; replacing it here keeps registration
        // ordering deterministic.
        // Slice 7: shared handle to the parent's live `current_run_id`.
        // Same Arc lives on the Agent and on the spawn tool; the
        // tool reads it at spawn time to stamp child runs with
        // `RunOrigin::Subagent { parent_run_id }`.
        let current_run_id: Arc<parking_lot::Mutex<Option<crate::run_record::RunId>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let spawn_tool = crate::tools::spawn::SpawnSubagentTool::with_store(
            Arc::clone(&config_arc),
            Arc::clone(&orchestration),
        )
        .with_artifact_store(Arc::clone(&artifact_store))
        .with_parent_session_id(session_id.clone())
        .with_parent_run_handle(Arc::clone(&current_run_id));
        tools.register(Arc::new(spawn_tool));

        Ok(Self {
            provider,
            provider_factory,
            tools,
            skills,
            context,
            memory,
            prompt_builder: PromptBuilder,
            hooks: Arc::new(hooks),
            session_id,
            turns: 0,
            turn_cancel: CancellationToken::new(),
            diff_sink,
            pricing,
            compaction_config: config.compaction.clone(),
            provider_config: active_provider.clone(),
            provider_api_key: secrecy::SecretString::from(api_key),
            active_profile: config.active_profile.clone(),
            lsp_manager,
            last_saved_count: 0,
            history_cache: Vec::new(),
            history_loaded: false,
            tool_context,
            max_iterations: max_iterations.unwrap_or(active_provider.max_iterations),
            auto_create_skills: config.auto_create_skills,
            orchestration: Arc::clone(&orchestration),
            tokens_consumed: 0,
            run_store,
            current_run_id,
            artifact_store,
            trust_profile,
        })
    }

    /// Borrow the shared edit-diff sink (YYC-66). The TUI clones this Arc
    /// and peeks the inner Option each frame to render the latest edit.
    pub fn diff_sink(&self) -> &crate::tools::EditDiffSink {
        &self.diff_sink
    }

    /// Per-token pricing for the configured model, when known (YYC-67).
    /// The TUI uses this with the cumulative token totals (YYC-60) to
    /// compute estimated session cost.
    pub fn pricing(&self) -> Option<&crate::provider::catalog::Pricing> {
        self.pricing.as_ref()
    }

    pub fn active_model(&self) -> &str {
        &self.provider_config.model
    }

    /// Name of the active named provider profile, or `None` when running
    /// on the legacy unnamed `[provider]` block (YYC-94).
    pub fn active_profile(&self) -> Option<&str> {
        self.active_profile.as_deref()
    }

    pub fn max_context(&self) -> usize {
        self.provider.max_context()
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Test/bench constructor that takes a fully-built provider and registry.
    /// Bypasses env-derived config so tests don't need a real API key and uses
    /// an in-memory session store.
    #[doc(hidden)]
    /// YYC-82: prune the agent's tool registry to the supplied
    /// allowlist. Called by `SpawnSubagentTool` after building a
    /// child agent so the child can't reach tools the parent
    /// didn't authorize. Tools not in the parent registry are
    /// silently ignored.
    pub fn restrict_tools(&mut self, allowed: &[String]) {
        self.tools.retain_only(allowed);
    }

    /// YYC-82: how many agent-loop iterations this Agent has run
    /// so far. Used by `SpawnSubagentTool` to report the child's
    /// budget usage back to the parent.
    pub fn iterations(&self) -> u32 {
        self.turns
    }

    /// YYC-206: handle to the orchestration store this Agent's
    /// `spawn_subagent` tool writes child runs into. The TUI
    /// clones the Arc to render real subagent records.
    pub fn orchestration(&self) -> Arc<crate::orchestration::OrchestrationStore> {
        Arc::clone(&self.orchestration)
    }

    /// YYC-211: cumulative `total_tokens` across every provider
    /// response since this Agent was built. Monotonic — readers
    /// diff snapshots for per-run usage.
    pub fn tokens_consumed(&self) -> u64 {
        self.tokens_consumed
    }

    /// YYC-179: handle to the run-record store this Agent appends
    /// turn lifecycle / provider / tool / hook events to. Cloned
    /// `Arc` so external readers (TUI debug pane, future
    /// `vulcan run show` CLI) share the same store.
    pub fn run_store(&self) -> Arc<dyn crate::run_record::RunStore> {
        Arc::clone(&self.run_store)
    }

    /// YYC-179: id of the run currently in flight, or `None` between
    /// turns. The TUI uses this to correlate live events with the
    /// timeline view.
    pub fn current_run_id(&self) -> Option<crate::run_record::RunId> {
        *self.current_run_id.lock()
    }

    /// Slice 7: cloneable handle to the per-Agent live `current_run_id`.
    /// Tools (notably `SpawnSubagentTool`) hold this Arc to read the
    /// parent's run id at the moment they fire, then stamp child
    /// runs with `RunOrigin::Subagent { parent_run_id }` so the
    /// timeline carries explicit lineage.
    pub fn current_run_id_handle(
        &self,
    ) -> Arc<parking_lot::Mutex<Option<crate::run_record::RunId>>> {
        Arc::clone(&self.current_run_id)
    }

    /// YYC-180: handle to the artifact store. Cloned `Arc` so the
    /// TUI / future `vulcan artifact` CLI / extensions read from
    /// the same backend the agent writes to.
    pub fn artifact_store(&self) -> Arc<dyn crate::artifact::ArtifactStore> {
        Arc::clone(&self.artifact_store)
    }

    /// YYC-193: snapshot of the tool definitions exposed to the
    /// LLM under the active profile. Used by contract tests to
    /// assert tool visibility without poking at the inner
    /// registry.
    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .definitions_with_context(Some(&self.tool_context))
    }

    /// YYC-182: resolved workspace trust profile. The TUI status
    /// area, run-record provenance, and `vulcan trust why` (later)
    /// all read from this.
    pub fn trust_profile(&self) -> &crate::trust::TrustProfile {
        &self.trust_profile
    }

    /// Slice 7: replace `spawn_subagent` with a daemon child-session
    /// runner for daemon-managed Agents. Direct-mode Agents keep the
    /// legacy runner-free tool installed by the builder.
    pub(crate) fn install_subagent_runner(
        &mut self,
        config: Arc<Config>,
        parent_session_id: impl Into<String>,
        runner: Arc<dyn crate::tools::spawn::SubagentRunner>,
    ) {
        let spawn_tool = crate::tools::spawn::SpawnSubagentTool::with_store(
            config,
            Arc::clone(&self.orchestration),
        )
        .with_artifact_store(Arc::clone(&self.artifact_store))
        .with_parent_session_id(parent_session_id)
        .with_parent_run_handle(Arc::clone(&self.current_run_id))
        .with_subagent_runner(runner);
        self.tools.register(Arc::new(spawn_tool));
    }

    /// YYC-180: persist a typed artifact and (when a run is in
    /// flight) emit a `RunEvent::ArtifactCreated` so the timeline
    /// references it. The artifact's `run_id` and `session_id` are
    /// auto-filled from the active run if the caller hasn't set
    /// them. Returns the stored artifact's id.
    pub fn create_artifact(
        &self,
        mut artifact: crate::artifact::Artifact,
    ) -> anyhow::Result<crate::artifact::ArtifactId> {
        let live_run_id = *self.current_run_id.lock();
        if artifact.run_id.is_none() {
            artifact.run_id = live_run_id;
        }
        if artifact.session_id.is_none() {
            artifact.session_id = Some(self.session_id.clone());
        }
        let id = artifact.id;
        let kind = artifact.kind;
        self.artifact_store.create(&artifact)?;
        // Emit on the run timeline if the agent is mid-turn so
        // `vulcan run show` lists the artifact alongside the events
        // that produced it.
        if let Some(run_id) = live_run_id {
            let _ = self.run_store.append_event(
                run_id,
                crate::run_record::RunEvent::ArtifactCreated {
                    artifact_id: id.to_string(),
                    artifact_type: kind.as_str().to_string(),
                },
            );
        }
        Ok(id)
    }

    pub fn for_test(
        provider: Box<dyn LLMProvider>,
        tools: ToolRegistry,
        hooks: HookRegistry,
        skills: Arc<SkillRegistry>,
    ) -> Self {
        let max_context = provider.max_context();
        Self {
            provider,
            provider_factory: Arc::new(DefaultProviderFactory),
            tools,
            skills,
            context: ContextManager::new(max_context),
            memory: Arc::new(SessionStore::in_memory()),
            prompt_builder: PromptBuilder,
            hooks: Arc::new(hooks),
            session_id: Uuid::new_v4().to_string(),
            turns: 0,
            turn_cancel: CancellationToken::new(),
            diff_sink: crate::tools::new_diff_sink(),
            pricing: None,
            compaction_config: CompactionConfig::default(),
            provider_config: ProviderConfig::default(),
            provider_api_key: secrecy::SecretString::from("test-key".to_string()),
            active_profile: None,
            lsp_manager: Arc::new(crate::code::lsp::LspManager::new(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )),
            last_saved_count: 0,
            history_cache: Vec::new(),
            history_loaded: false,
            max_iterations: 0,
            tool_context: crate::tools::ToolContext::probe(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            ),
            auto_create_skills: false,
            orchestration: Arc::new(crate::orchestration::OrchestrationStore::new()),
            tokens_consumed: 0,
            run_store: Arc::new(crate::run_record::InMemoryRunStore::default()),
            current_run_id: Arc::new(parking_lot::Mutex::new(None)),
            artifact_store: Arc::new(crate::artifact::InMemoryArtifactStore::new()),
            trust_profile: crate::trust::TrustProfile::for_level_with_reason(
                crate::trust::TrustLevel::Trusted,
                "test fixture",
            ),
        }
    }

    /// Cancel the currently-running turn. Safe to call from any thread.
    /// After cancellation fires, the in-flight tool/LLM/hook futures get
    /// dropped (with `kill_on_drop` semantics where applicable) and the
    /// agent loop exits cleanly. The next `run_prompt` call swaps in a
    /// fresh token, so this only affects the current turn.
    pub fn cancel_current_turn(&self) {
        self.turn_cancel.cancel();
    }

    /// Clone the underlying turn-cancellation token. Cheap; safe to hold
    /// outside the agent's lock and to fire from any task. The daemon
    /// stashes this on `SessionState` so `prompt.cancel` can fire
    /// cancellation without ever locking the AsyncMutex that
    /// `prompt.stream` holds for the duration of a turn.
    pub fn cancel_handle(&self) -> CancellationToken {
        self.turn_cancel.clone()
    }

    /// Borrow the shared LSP manager (YYC-46). Used by the
    /// auto-diagnostics hook (YYC-51) to query post-edit diagnostics
    /// without re-spawning servers.
    pub fn lsp_manager(&self) -> &Arc<crate::code::lsp::LspManager> {
        &self.lsp_manager
    }

    /// Loaded skills for the active session (YYC-37 /skills slash).
    pub fn skills(&self) -> &[crate::skills::Skill] {
        self.skills.list()
    }

    /// Save only new messages since the last save, avoiding the O(n) DELETE +
    /// re-INSERT that `save_messages` does. Tracks `last_saved_count` so
    /// subsequent calls only persist `messages[last_saved_count..]`.
    ///
    /// YYC-138: when compaction rewrites the in-memory `messages` Vec in
    /// place (replacing N old entries with a 2-entry summary), this
    /// method's `messages.len()` shrinks below `self.last_saved_count`.
    /// In that case we *replace* the persisted snapshot wholesale —
    /// otherwise the next `>` append would slice the wrong tail and
    /// orphan Tool rows from the pre-compaction history, which the
    /// provider rejects on the next turn ("Tool message must follow
    /// Assistant tool_calls"). Use [`Self::replace_history`] for the
    /// explicit reset call sites; this auto-detect is a defense.
    pub fn save_messages(&mut self, messages: &[Message]) -> Result<()> {
        let new_count = messages.len();
        if new_count < self.last_saved_count {
            self.memory.save_messages(&self.session_id, messages)?;
            self.last_saved_count = new_count;
            // Slice 2: keep the in-memory snapshot in lockstep with
            // storage so a compaction-style shrink (handled here as a
            // defensive fallback) does not drift the cache from
            // durability.
            self.history_cache = messages.iter().skip(1).cloned().collect();
        } else if new_count > self.last_saved_count {
            let to_save = &messages[self.last_saved_count..];
            self.memory.append_messages(&self.session_id, to_save)?;
            // Slice 2: extend the cache with the just-appended tail
            // so subsequent turns observe live conversation state
            // without re-running `load_history`.
            self.history_cache.extend_from_slice(to_save);
            self.last_saved_count = new_count;
        }
        Ok(())
    }

    /// Replace the persisted history for the active session with the
    /// supplied `messages` snapshot and reset the incremental save
    /// cursor. Use this after compaction or any other in-place rewrite
    /// so subsequent `save_messages` calls append on top of the new
    /// truncated history rather than leaving orphan Tool rows behind
    /// (YYC-138).
    pub fn replace_history(&mut self, messages: &[Message]) -> Result<()> {
        self.memory.save_messages(&self.session_id, messages)?;
        self.last_saved_count = messages.len();
        // Slice 2: cache mirrors the post-rewrite snapshot. Skip the
        // leading System frame because it's rebuilt fresh by the
        // prompt builder on each `prepare_turn`.
        self.history_cache = messages.iter().skip(1).cloned().collect();
        self.history_loaded = true;
        Ok(())
    }
}

pub(in crate::agent) fn flatten_for_message(result: ToolResult) -> String {
    if let Some(details) = result.details {
        return serde_json::json!({
            "output": result.output,
            "details": details,
            "media": result.media,
            "is_error": result.is_error,
        })
        .to_string();
    }
    if result.media.is_empty() {
        return result.output;
    }
    let media_block = result
        .media
        .iter()
        .map(|m| format!("[media: {m}]"))
        .collect::<Vec<_>>()
        .join("\n");
    if result.output.is_empty() {
        media_block
    } else {
        format!("{}\n\n{media_block}", result.output)
    }
}
