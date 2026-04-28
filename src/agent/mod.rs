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
    pub(in crate::agent) tools: ToolRegistry,
    pub(in crate::agent) skills: Arc<SkillRegistry>,
    pub(in crate::agent) context: ContextManager,
    pub(in crate::agent) memory: SessionStore,
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
    pub(in crate::agent) provider_api_key: String,
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

    pub async fn build(self) -> Result<Agent> {
        Agent::build_from_parts(self.config, self.hooks, self.pause_tx, self.max_iterations).await
    }
}

impl Agent {
    pub fn builder(config: &Config) -> AgentBuilder<'_> {
        AgentBuilder {
            config,
            hooks: HookRegistry::new(),
            pause_tx: None,
            max_iterations: None,
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
    ) -> Result<Self> {
        // Local / self-hosted endpoints don't require auth; allow empty
        // string in that case (matches `switch_provider` semantics).
        let api_key = match config.api_key() {
            Some(k) => k,
            None if config.provider.disable_catalog
                || is_local_base_url(&config.provider.base_url) =>
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
        let selection = Self::resolve_model_selection(&config.provider, &api_key).await?;
        let effective_max_context = selection.max_context;
        let supports_json_mode = selection.model.features.json_mode;
        let pricing = selection.pricing.clone();

        let provider_factory: Arc<dyn ProviderFactory> = Arc::new(DefaultProviderFactory);
        let provider = provider_factory.build(
            &config.provider,
            &api_key,
            effective_max_context,
            supports_json_mode,
        )?;

        let diff_sink = crate::tools::new_diff_sink();
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let lsp_manager = Arc::new(crate::code::lsp::LspManager::new(cwd.clone()));
        // YYC-107: probe the workspace once so tool registration can
        // drop irrelevant tools (cargo_check off-Rust, etc.) and the
        // remaining tools can render runtime-aware descriptions.
        let tool_context = crate::tools::ToolContext::probe(cwd.clone());
        let mut tools = ToolRegistry::new_with_diff_and_lsp(
            Some(diff_sink.clone()),
            Some(lsp_manager.clone()),
            cwd,
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
        let config_arc = Arc::new(config.clone());
        let orchestration = Arc::new(crate::orchestration::OrchestrationStore::new());
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
                config.provider.base_url.clone(),
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
        let skills = Arc::new(SkillRegistry::new(&config.skills_dir));
        let memory = SessionStore::try_new()?;
        let context =
            ContextManager::with_config(provider.max_context(), config.compaction.clone());
        let session_id = Uuid::new_v4().to_string();

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

        // YYC-107: drop tools that aren't relevant to this workspace.
        tools.filter_for_context(&tool_context);

        Ok(Self {
            provider,
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
            provider_config: config.provider.clone(),
            provider_api_key: api_key,
            active_profile: None,
            lsp_manager,
            last_saved_count: 0,
            tool_context,
            max_iterations: max_iterations.unwrap_or(config.provider.max_iterations),
            auto_create_skills: config.auto_create_skills,
            orchestration: Arc::clone(&orchestration),
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

    pub fn for_test(
        provider: Box<dyn LLMProvider>,
        tools: ToolRegistry,
        hooks: HookRegistry,
        skills: Arc<SkillRegistry>,
    ) -> Self {
        let max_context = provider.max_context();
        Self {
            provider,
            tools,
            skills,
            context: ContextManager::new(max_context),
            memory: SessionStore::in_memory(),
            prompt_builder: PromptBuilder,
            hooks: Arc::new(hooks),
            session_id: Uuid::new_v4().to_string(),
            turns: 0,
            turn_cancel: CancellationToken::new(),
            diff_sink: crate::tools::new_diff_sink(),
            pricing: None,
            compaction_config: CompactionConfig::default(),
            provider_config: ProviderConfig::default(),
            provider_api_key: "test-key".into(),
            active_profile: None,
            lsp_manager: Arc::new(crate::code::lsp::LspManager::new(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )),
            last_saved_count: 0,
            max_iterations: 0,
            tool_context: crate::tools::ToolContext::probe(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            ),
            auto_create_skills: false,
            orchestration: Arc::new(crate::orchestration::OrchestrationStore::new()),
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
        } else if new_count > self.last_saved_count {
            let to_save = &messages[self.last_saved_count..];
            self.memory.append_messages(&self.session_id, to_save)?;
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
        Ok(())
    }
}

pub(in crate::agent) fn flatten_for_message(result: ToolResult) -> String {
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
