use std::sync::Arc;

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::config::{Config, ProviderConfig};
use crate::context::ContextManager;
use crate::hooks::HookRegistry;
use crate::hooks::approval::ApprovalHook;
use crate::hooks::diagnostics::DiagnosticsHook;
use crate::hooks::safety::SafetyHook;
use crate::hooks::skills::SkillsHook;
use crate::memory::SessionStore;
use crate::pause::PauseSender;
use crate::prompt_builder::PromptBuilder;
use crate::provider::openai::OpenAIProvider;
use crate::provider::{LLMProvider, Message};
use crate::skills::SkillRegistry;
use crate::tools::{ToolRegistry, ToolResult};

mod dispatch;
mod provider;
mod run;
mod session;

#[cfg(test)]
mod tests;

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
}

#[derive(Debug, Clone)]
pub struct ModelSelection {
    pub model: crate::provider::catalog::ModelInfo,
    pub max_context: usize,
    pub pricing: Option<crate::provider::catalog::Pricing>,
}

impl Agent {
    /// Construct an Agent with no caller-supplied hooks. Built-in hooks (skills
    /// injection, etc.) are still registered.
    pub async fn new(config: &Config) -> Result<Self> {
        Self::with_hooks_and_pause(config, HookRegistry::new(), None).await
    }

    /// Construct with caller-supplied hooks and no interactive pause channel.
    /// The TUI uses `with_hooks_and_pause` to wire one up.
    pub async fn with_hooks(config: &Config, hooks: HookRegistry) -> Result<Self> {
        Self::with_hooks_and_pause(config, hooks, None).await
    }

    /// Construct an Agent with a caller-supplied hook registry and an optional
    /// pause emitter. Built-ins (skills, safety) are registered into the
    /// registry; if a pause emitter is provided, `SafetyHook` is wired up to
    /// route blocks through it as `AgentPause::SafetyApproval`.
    ///
    /// Async because it fetches the provider's model catalog at startup
    /// (YYC-64). Catalog-fetch failures are non-fatal — logged and continued
    /// with config defaults.
    ///
    /// Returns `Err` for fatal init failures (missing API key, provider build
    /// failure, or model not found in catalog).
    pub async fn with_hooks_and_pause(
        config: &Config,
        mut hooks: HookRegistry,
        pause_tx: Option<PauseSender>,
    ) -> Result<Self> {
        let api_key = config.api_key().ok_or_else(|| {
            anyhow::anyhow!(
                "No API key configured. Set VULCAN_API_KEY or add api_key to ~/.vulcan/config.toml"
            )
        })?;

        // ── Catalog: validate the configured model and (optionally) override
        // max_context with whatever the catalog says it actually is. Non-fatal:
        // if the catalog endpoint fails, we log + continue with the configured
        // values rather than blocking startup over a metadata fetch.
        let selection = Self::resolve_model_selection(&config.provider, &api_key).await?;
        let effective_max_context = selection.max_context;
        let supports_json_mode = selection.model.features.json_mode;
        let pricing = selection.pricing.clone();

        let provider: Box<dyn LLMProvider> = Box::new(
            OpenAIProvider::new(
                &config.provider.base_url,
                &api_key,
                &config.provider.model,
                effective_max_context,
                config.provider.max_retries,
                supports_json_mode,
                config.provider.debug,
            )
            .context("Failed to initialize LLM provider")?,
        );

        let diff_sink = crate::tools::new_diff_sink();
        let lsp_manager = Arc::new(crate::code::lsp::LspManager::new(
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        ));
        let mut tools =
            ToolRegistry::new_with_diff_and_lsp(Some(diff_sink.clone()), Some(lsp_manager.clone()));

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
        let memory = SessionStore::new();
        let context = ContextManager::new(provider.max_context());
        let session_id = Uuid::new_v4().to_string();

        // Built-in hook: surface available skills to the LLM via BeforePrompt.
        hooks.register(Arc::new(SkillsHook::new(skills.clone())));

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
            let safety = match pause_tx {
                Some(tx) => SafetyHook::with_pause_emitter(tx),
                None => SafetyHook::new(),
            };
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
            provider_config: config.provider.clone(),
            provider_api_key: api_key,
            active_profile: None,
            lsp_manager,
            last_saved_count: 0,
            max_iterations: config.provider.max_iterations,
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

    /// Test/bench-only constructor that takes a fully-built provider and an
    /// empty (or test-curated) registry. Bypasses the env-derived config
    /// path so tests don't need a real API key. Memory points at an in-memory or
    /// temporary store via the caller — this constructor leaves the real
    /// `SessionStore::new()` path which writes to ~/.vulcan; tests should
    /// override `Agent::memory` if they care about isolation, or pass a
    /// custom session_id and accept that ~/.vulcan/sessions.db gets touched.
    #[cfg(any(test, feature = "bench-soak"))]
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
            provider_config: ProviderConfig::default(),
            provider_api_key: "test-key".into(),
            active_profile: None,
            lsp_manager: Arc::new(crate::code::lsp::LspManager::new(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )),
            last_saved_count: 0,
            max_iterations: 0,
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

    /// Borrow the underlying `SessionStore`. Used by the TUI's `/search`
    /// command and the `vulcan search` CLI subcommand to run FTS queries.
    pub fn memory(&self) -> &crate::memory::SessionStore {
        &self.memory
    }

    /// Save only new messages since the last save, avoiding the O(n) DELETE +
    /// re-INSERT that `save_messages` does. Tracks `last_saved_count` so
    /// subsequent calls only persist `messages[last_saved_count..]`.
    pub fn save_messages(&mut self, messages: &[Message]) -> Result<()> {
        let new_count = messages.len();
        if new_count > self.last_saved_count {
            let to_save = &messages[self.last_saved_count..];
            self.memory.append_messages(&self.session_id, to_save)?;
            self.last_saved_count = new_count;
        }
        Ok(())
    }
}

/// Render a `ToolResult` to the `String` payload that goes into
/// `Message::Tool { content }`. Media references are inlined as `[media: ...]`
/// markers since the OpenAI tool message format only carries a single text
/// field.
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
