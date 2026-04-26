use std::sync::Arc;

use crate::config::Config;
use crate::context::ContextManager;
use crate::hooks::approval::ApprovalHook;
use crate::hooks::diagnostics::DiagnosticsHook;
use crate::hooks::safety::SafetyHook;
use crate::hooks::skills::SkillsHook;
use crate::hooks::{HookRegistry, ToolCallDecision};
use crate::memory::SessionStore;
use crate::pause::PauseSender;
use crate::prompt_builder::PromptBuilder;
use crate::provider::openai::OpenAIProvider;
use crate::provider::{LLMProvider, Message, StreamEvent};
use crate::skills::SkillRegistry;
use crate::tools::{ToolRegistry, ToolResult};
use anyhow::{Context, Result};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// The core agent — orchestrates the LLM, tools, hooks, and state.
///
/// One Agent per session. Hold it across turns: the hook registry's stateful
/// handlers (audit log, rate limits, approval caches) only work if the Agent
/// outlives a single prompt.
pub struct Agent {
    provider: Box<dyn LLMProvider>,
    tools: ToolRegistry,
    skills: Arc<SkillRegistry>,
    context: ContextManager,
    memory: SessionStore,
    prompt_builder: PromptBuilder,
    hooks: Arc<HookRegistry>,
    session_id: String,
    turns: u32,
    /// Per-turn cancellation token. `cancel_current_turn()` fires it; the
    /// next call to `run_prompt` / `run_prompt_stream` swaps in a fresh token
    /// so cancel applies to the in-flight turn only, not future ones.
    turn_cancel: CancellationToken,
    /// Latest file edit, captured by `WriteFile`/`PatchFile` (YYC-66).
    /// TUI clones this Arc and renders the inner Option each frame.
    diff_sink: crate::tools::EditDiffSink,
    /// Per-token pricing for the active model, sourced from the provider
    /// catalog at startup (YYC-67). `None` when the catalog is disabled
    /// or the provider doesn't publish pricing.
    pricing: Option<crate::provider::catalog::Pricing>,
    /// LSP server pool (YYC-46). Lazy: servers spawn on first tool
    /// invocation that needs one. Reaped in `end_session`.
    lsp_manager: Arc<crate::code::lsp::LspManager>,
    /// Number of messages in `run_prompt[_stream]`'s `messages` Vec that have
    /// already been persisted to SQLite. Used to skip the O(n) DELETE + re-INSERT
    /// on every turn — only `messages[last_saved_count..]` are new (YYC-76).
    last_saved_count: usize,
    /// Max agent loop iterations per prompt. 0 = unlimited (default).
    max_iterations: u32,
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
        let mut effective_max_context = config.provider.max_context;
        let mut supports_json_mode = false;
        let mut pricing: Option<crate::provider::catalog::Pricing> = None;
        if !config.provider.disable_catalog {
            match Self::fetch_catalog(config, &api_key).await {
                Ok(models) => {
                    let found = models.iter().find(|m| m.id == config.provider.model);
                    match found {
                        Some(model_info) => {
                            supports_json_mode = model_info.features.json_mode;
                            pricing = model_info.pricing.clone();
                            if model_info.context_length > 0
                                && config.provider.max_context == 128_000
                            {
                                effective_max_context = model_info.context_length;
                                tracing::info!(
                                    "catalog: using context_length={} for {} (json_mode={})",
                                    model_info.context_length,
                                    model_info.id,
                                    supports_json_mode,
                                );
                            }
                        }
                        None => {
                            let suggestions = crate::provider::catalog::fuzzy_suggest(
                                &models,
                                &config.provider.model,
                                3,
                            );
                            let hint = if suggestions.is_empty() {
                                String::new()
                            } else {
                                format!(" Did you mean: {}?", suggestions.join(", "))
                            };
                            anyhow::bail!(
                                "Model '{}' not found in provider catalog.{} \
                                 (See `[provider].model` in config.)",
                                config.provider.model,
                                hint,
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("catalog fetch failed (continuing with config defaults): {e}");
                }
            }
        }

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
        let mut tools = ToolRegistry::new_with_diff_and_lsp(
            Some(diff_sink.clone()),
            Some(lsp_manager.clone()),
        );

        // YYC-81: ask_user is only useful in interactive (TUI) mode.
        // Register it whenever a pause channel is wired; it self-
        // reports when called without one.
        if pause_tx.is_some() {
            tools.register(Arc::new(crate::tools::ask_user::AskUserTool::new(
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
        hooks.register(Arc::new(ApprovalHook::new(approval_cfg, pause_tx.clone())));

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

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Fetch the provider's model catalog (cached if fresh, otherwise
    /// HTTP-fetched). Used for startup validation and `max_context`
    /// auto-population. Runs inside the caller's tokio runtime — the
    /// constructor is `async` for exactly this reason.
    async fn fetch_catalog(
        config: &Config,
        api_key: &str,
    ) -> Result<Vec<crate::provider::catalog::ModelInfo>> {
        use std::time::Duration;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        let ttl = Duration::from_secs(config.provider.catalog_cache_ttl_hours * 3600);
        let catalog =
            crate::provider::catalog::for_base_url(client, &config.provider.base_url, api_key, ttl);
        catalog.list_models().await.map_err(Into::into)
    }

    /// Test-only constructor that takes a fully-built provider and an empty
    /// (or test-curated) registry. Bypasses the env-derived config path so
    /// tests don't need a real API key. Memory points at an in-memory or
    /// temporary store via the caller — this constructor leaves the real
    /// `SessionStore::new()` path which writes to ~/.vulcan; tests should
    /// override `Agent::memory` if they care about isolation, or pass a
    /// custom session_id and accept that ~/.vulcan/sessions.db gets touched.
    #[cfg(test)]
    pub(crate) fn for_test(
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

    /// Fires `session_start` on all hook handlers. Call once after construction
    /// (Agent::new doesn't call it itself because hooks aren't always async-
    /// available at construction time).
    pub async fn start_session(&self) {
        if let Err(e) = self
            .memory
            .save_session_metadata(&self.session_id, None, None)
        {
            tracing::warn!("failed to initialize session metadata: {e}");
        }
        self.hooks.session_start(&self.session_id).await;
    }

    /// Fires `session_end` and records the total turn count. Also
    /// reaps any LSP servers spawned during the session (YYC-46).
    pub async fn end_session(&self) {
        self.hooks.session_end(&self.session_id, self.turns).await;
        self.lsp_manager.shutdown_all().await;
    }

    /// Borrow the shared LSP manager (YYC-46). Used by the
    /// auto-diagnostics hook (YYC-51) to query post-edit diagnostics
    /// without re-spawning servers.
    pub fn lsp_manager(&self) -> &Arc<crate::code::lsp::LspManager> {
        &self.lsp_manager
    }

    /// Run a one-shot prompt (no TUI). Gathers context, calls LLM, dispatches
    /// tools, returns result. Honors all hook events.
    pub async fn run_prompt(&mut self, input: &str) -> Result<String> {
        // Fresh token for this turn — calling cancel_current_turn between
        // turns shouldn't affect the next one.
        self.turn_cancel = CancellationToken::new();
        let cancel = self.turn_cancel.clone();

        let system = self.prompt_builder.build_system_prompt(&self.tools);
        let tool_defs = self.tools.definitions();
        let mut messages = vec![Message::System { content: system }];

        // Load history for *this* agent's session — set by `resume_session` or
        // `continue_last_session`; defaults to a fresh UUID so a new agent has
        // empty history.
        if let Some(history) = self.memory.load_history(&self.session_id)? {
            for msg in history {
                messages.push(msg);
            }
        }

        messages.push(Message::User {
            content: input.to_string(),
        });

        let max_iter = if self.max_iterations > 0 {
            self.max_iterations as usize
        } else {
            usize::MAX
        };
        for iteration in 0..max_iter {
            tracing::debug!("Agent iteration {iteration}");

            if self.context.should_compact(&messages) {
                let summary = self.context.compact(&messages)?;
                messages = vec![
                    Message::System {
                        content: format!("Previous conversation context:\n{summary}"),
                    },
                    Message::User {
                        content: input.to_string(),
                    },
                ];
            }

            if cancel.is_cancelled() {
                return Ok("Cancelled".to_string());
            }

            // ── BeforePrompt: handlers may inject extra messages. Injections
            // are transient — they go on the wire but don't persist into the
            // conversation history we save to memory.
            let outgoing = self
                .hooks
                .apply_before_prompt(&messages, cancel.clone())
                .await;

            let response = self
                .provider
                .chat(&outgoing, &tool_defs, cancel.clone())
                .await?;

            if let Some(usage) = &response.usage {
                self.context
                    .record_usage(usage.prompt_tokens, usage.completion_tokens);
            }

            if let Some(tool_calls) = &response.tool_calls {
                messages.push(Message::Assistant {
                    content: response.content.clone(),
                    tool_calls: Some(tool_calls.clone()),
                    reasoning_content: response.reasoning_content.clone(),
                });

                // YYC-34: dispatch all calls concurrently. Each dispatch still
                // runs through BeforeToolCall + AfterToolCall hooks via
                // `dispatch_tool`. Errors are isolated — a failing tool yields
                // a `ToolResult::err`, never aborts the others. `join_all`
                // preserves order so messages line up with their tool_call_id.
                let this: &Self = self;
                let dispatches = tool_calls.iter().map(|tc| {
                    let id = tc.id.clone();
                    let name = tc.function.name.clone();
                    let args = tc.function.arguments.clone();
                    let cancel = cancel.clone();
                    async move {
                        tracing::info!("Executing tool: {name} (call {id})");
                        let result = this.dispatch_tool(&name, &args, cancel).await;
                        (id, result)
                    }
                });
                let results = futures_util::future::join_all(dispatches).await;
                for (id, result) in results {
                    messages.push(Message::Tool {
                        tool_call_id: id,
                        content: flatten_for_message(result),
                    });
                }
            } else {
                let text = response.content.unwrap_or_default();
                let reasoning = response.reasoning_content.clone();

                // ── BeforeAgentEnd: a handler may force the loop to continue.
                if let Some(instruction) = self.hooks.before_agent_end(&text, cancel.clone()).await
                {
                    messages.push(Message::Assistant {
                        content: Some(text.clone()),
                        tool_calls: None,
                        reasoning_content: reasoning,
                    });
                    messages.push(Message::User {
                        content: instruction,
                    });
                    continue;
                }

                self.save_messages(&messages)?;
                self.turns = self.turns.saturating_add(1);
                if iteration >= 5 {
                    self.skills.try_auto_create(input, &text)?;
                }
                return Ok(text);
            }
        }

        Ok("Agent reached maximum iteration limit.".to_string())
    }

    /// Run a prompt with streaming — sends text tokens through `ui_tx` as they
    /// arrive. Honors all hook events.
    pub async fn run_prompt_stream(
        &mut self,
        input: &str,
        ui_tx: mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<String> {
        // Fresh per-turn cancel token (see run_prompt).
        self.turn_cancel = CancellationToken::new();
        let cancel = self.turn_cancel.clone();

        let system = self.prompt_builder.build_system_prompt(&self.tools);
        let tool_defs = self.tools.definitions();
        let mut messages = vec![Message::System { content: system }];

        if let Some(history) = self.memory.load_history(&self.session_id)? {
            for msg in history {
                messages.push(msg);
            }
        }

        messages.push(Message::User {
            content: input.to_string(),
        });

        let mut full_response = String::new();

        let max_iter = if self.max_iterations > 0 {
            self.max_iterations as usize
        } else {
            usize::MAX
        };
        for iteration in 0..max_iter {
            if cancel.is_cancelled() {
                let _ = ui_tx.send(StreamEvent::Done(crate::provider::ChatResponse {
                    content: Some("Cancelled".into()),
                    tool_calls: None,
                    usage: None,
                    finish_reason: Some("cancelled".into()),
                    reasoning_content: None,
                }));
                return Ok("Cancelled".to_string());
            }

            tracing::info!(
                "agent iteration {iteration} starting (messages={})",
                messages.len()
            );

            // ── BeforePrompt (transient — see run_prompt for rationale).
            let outgoing = self
                .hooks
                .apply_before_prompt(&messages, cancel.clone())
                .await;

            let (inner_tx, mut inner_rx) = mpsc::unbounded_channel::<StreamEvent>();
            let (priv_tx, mut priv_rx) = mpsc::unbounded_channel::<StreamEvent>();

            let ui_tx_clone = ui_tx.clone();
            tokio::spawn(async move {
                while let Some(ev) = inner_rx.recv().await {
                    match &ev {
                        StreamEvent::Text(_) => {
                            let _ = ui_tx_clone.send(ev);
                        }
                        StreamEvent::Done(_) | StreamEvent::Error(_) => {
                            let _ = priv_tx.send(ev);
                            break;
                        }
                        _ => {
                            let _ = ui_tx_clone.send(ev);
                        }
                    }
                }
            });

            if let Err(e) = self
                .provider
                .chat_stream(&outgoing, &tool_defs, inner_tx, cancel.clone())
                .await
            {
                // Surface provider failures to the TUI rather than dropping
                // the channel silently. ProviderError carries actionable hints
                // via its Display impl; if the error chain has one (most
                // common case), use that — otherwise fall back to the raw
                // anyhow chain.
                let user_message = e
                    .downcast_ref::<crate::provider::ProviderError>()
                    .map(|pe| pe.to_string())
                    .unwrap_or_else(|| format!("{e}"));
                tracing::error!("agent iteration {iteration}: chat_stream failed: {user_message}");
                let _ = ui_tx.send(StreamEvent::Error(user_message.clone()));
                let _ = ui_tx.send(StreamEvent::Done(crate::provider::ChatResponse {
                    content: Some(format!("⚠ {user_message}")),
                    tool_calls: None,
                    usage: None,
                    finish_reason: Some("error".into()),
                    reasoning_content: None,
                }));
                return Err(e);
            }

            let mut final_response: Option<crate::provider::ChatResponse> = None;
            while let Some(event) = priv_rx.recv().await {
                match event {
                    StreamEvent::Done(resp) => {
                        final_response = Some(resp);
                        break;
                    }
                    StreamEvent::Error(e) => {
                        return Err(anyhow::anyhow!("{e}"));
                    }
                    _ => {}
                }
            }

            let response = match final_response {
                Some(r) => r,
                None => {
                    let msg = "Stream ended without Done event";
                    tracing::error!("agent iteration {iteration}: {msg}");
                    let _ = ui_tx.send(StreamEvent::Error(msg.into()));
                    return Err(anyhow::anyhow!(msg));
                }
            };

            tracing::info!(
                "agent iteration {iteration}: response content_len={}, tool_calls={}, reasoning_len={}",
                response.content.as_deref().map(|s| s.len()).unwrap_or(0),
                response.tool_calls.as_ref().map(|t| t.len()).unwrap_or(0),
                response
                    .reasoning_content
                    .as_deref()
                    .map(|s| s.len())
                    .unwrap_or(0),
            );

            if let Some(text) = &response.content {
                full_response.push_str(text);
            }

            if let Some(tool_calls) = &response.tool_calls {
                messages.push(Message::Assistant {
                    content: response.content.clone(),
                    tool_calls: Some(tool_calls.clone()),
                    reasoning_content: response.reasoning_content.clone(),
                });

                // YYC-34: dispatch all calls concurrently. ToolCallStart events
                // fire synchronously up front so the TUI shows every in-flight
                // tool immediately; ToolCallEnd fires from inside each future
                // as it completes (so they may arrive out of order — that's the
                // point). `join_all` preserves order for the message vector
                // so tool_call_id alignment stays deterministic.
                for tc in tool_calls {
                    // YYC-74: derive a one-line args summary so the TUI's
                    // tool-card has structured context to show.
                    let args_summary =
                        summarize_tool_args(&tc.function.name, &tc.function.arguments);
                    let _ = ui_tx.send(StreamEvent::ToolCallStart {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        args_summary,
                    });
                }
                let this: &Self = self;
                let dispatches = tool_calls.iter().map(|tc| {
                    let id = tc.id.clone();
                    let name = tc.function.name.clone();
                    let args = tc.function.arguments.clone();
                    let cancel = cancel.clone();
                    let ui_tx = ui_tx.clone();
                    async move {
                        tracing::info!("Executing tool: {name} (call {id})");
                        let started = std::time::Instant::now();
                        let result = this.dispatch_tool(&name, &args, cancel).await;
                        let elapsed_ms = started.elapsed().as_millis() as u64;
                        let ok = !result.is_error;
                        // YYC-74: truncated output preview + meta line.
                        // YYC-78: elided line count for the auto-collapse
                        // "N more lines" indicator.
                        // Tools like write_file/edit_file populate
                        // `display_preview` with a real diff; prefer it
                        // over the LLM-facing terse `output`.
                        let preview_source = result
                            .display_preview
                            .as_deref()
                            .unwrap_or(&result.output);
                        let output_preview = preview_output(preview_source);
                        let result_meta = summarize_tool_result(&name, &result.output);
                        let elided = elided_lines(preview_source, output_preview.as_deref());
                        let _ = ui_tx.send(StreamEvent::ToolCallEnd {
                            id: id.clone(),
                            name: name.clone(),
                            ok,
                            output_preview,
                            result_meta,
                            elided_lines: elided,
                            elapsed_ms,
                        });
                        (id, result)
                    }
                });
                let results = futures_util::future::join_all(dispatches).await;
                for (id, result) in results {
                    messages.push(Message::Tool {
                        tool_call_id: id,
                        content: flatten_for_message(result),
                    });
                }
            } else {
                let reasoning = response.reasoning_content.clone();

                // ── BeforeAgentEnd
                if let Some(instruction) = self
                    .hooks
                    .before_agent_end(&full_response, cancel.clone())
                    .await
                {
                    messages.push(Message::Assistant {
                        content: Some(full_response.clone()),
                        tool_calls: None,
                        reasoning_content: reasoning,
                    });
                    messages.push(Message::User {
                        content: instruction,
                    });
                    continue;
                }

                // Final assistant turn after the loop ends — save with reasoning
                // so the next turn can echo it back to thinking-mode models.
                messages.push(Message::Assistant {
                    content: Some(full_response.clone()),
                    tool_calls: None,
                    reasoning_content: reasoning.clone(),
                });

                self.save_messages(&messages)?;
                self.turns = self.turns.saturating_add(1);
                if iteration >= 5 {
                    self.skills.try_auto_create(input, &full_response)?;
                }

                // If the model returned an empty response, surface it via a
                // synthetic Text event so the user sees *something* rather
                // than the chat appearing frozen on the previous marker.
                if full_response.is_empty() {
                    tracing::warn!(
                        "agent iteration {iteration}: model returned empty content with no tool calls"
                    );
                    let _ = ui_tx.send(StreamEvent::Text(
                        "_(model returned empty response)_".into(),
                    ));
                }

                let _ = ui_tx.send(StreamEvent::Done(crate::provider::ChatResponse {
                    content: Some(full_response.clone()),
                    tool_calls: None,
                    usage: response.usage,
                    finish_reason: response.finish_reason,
                    reasoning_content: reasoning,
                }));
                return Ok(full_response);
            }
        }

        // Send a Done event so the TUI exits thinking mode, even
        // though there's no text-only final turn. The loop maxed out
        // at 10 iterations of tool calls — without this, the UI hangs
        // in thinking=true forever (YYC-76).
        let _ = ui_tx.send(StreamEvent::Text(
            "Agent reached maximum iteration limit.".into(),
        ));
        let _ = ui_tx.send(StreamEvent::Done(crate::provider::ChatResponse {
            content: Some("Agent reached maximum iteration limit.".into()),
            tool_calls: None,
            usage: None,
            finish_reason: Some("max_iterations".into()),
            reasoning_content: None,
        }));
        Ok("Agent reached maximum iteration limit.".to_string())
    }

    /// Resume a previous session by ID. Swaps `self.session_id` to the
    /// requested one; subsequent `run_prompt[_stream]` calls load and append
    /// to that session's history. Errors if the session doesn't exist.
    pub fn resume_session(&mut self, session_id: &str) -> Result<()> {
        let history = self
            .memory
            .load_history(session_id)?
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;
        self.session_id = session_id.to_string();
        tracing::info!("resumed session {session_id} ({} messages)", history.len());
        Ok(())
    }

    /// Resume the most recently active session. Errors if there are no
    /// sessions on disk.
    pub fn continue_last_session(&mut self) -> Result<()> {
        match self.memory.last_session_id() {
            Some(id) => self.resume_session(&id),
            None => anyhow::bail!("No previous session to resume"),
        }
    }

    /// Create a new child session rooted at the current one, persist its
    /// lineage, and switch the agent to that child session immediately.
    pub fn fork_session(&mut self, lineage_label: Option<&str>) -> Result<String> {
        let parent_session_id = self.session_id.clone();
        let child_session_id = Uuid::new_v4().to_string();
        self.memory.save_session_metadata(
            &child_session_id,
            Some(&parent_session_id),
            lineage_label,
        )?;
        self.session_id = child_session_id.clone();
        tracing::info!(
            "forked session {} -> {}",
            parent_session_id,
            child_session_id
        );
        Ok(child_session_id)
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

    /// Dispatch a single tool call, running BeforeToolCall + AfterToolCall
    /// hooks around it. Returns the result flattened to the `String` payload
    /// expected by `Message::Tool` (media references inlined as `[media: ...]`
    /// markers). Hooks see the full `ToolResult`.
    /// Run BeforeToolCall + tool dispatch + AfterToolCall hooks. Returns the
    /// final `ToolResult` so callers can both flatten it for `Message::Tool`
    /// and inspect `is_error` (e.g. to emit `StreamEvent::ToolCallEnd { ok }`).
    async fn dispatch_tool(
        &self,
        name: &str,
        raw_args: &str,
        cancel: CancellationToken,
    ) -> ToolResult {
        let parsed_args: Value = match serde_json::from_str(raw_args) {
            Ok(v) => v,
            Err(e) => {
                // Hooks see Null when args are unparseable, but we surface the
                // structured error to the LLM via `tools.execute` (which also
                // re-parses) so the agent can self-correct on the next turn.
                tracing::warn!(
                    "Tool '{name}' received unparseable JSON args ({e}). Raw: {raw_args}"
                );
                Value::Null
            }
        };

        let (effective_args_str, blocked) = match self
            .hooks
            .before_tool_call(name, &parsed_args, cancel.clone())
            .await
        {
            ToolCallDecision::Continue => (raw_args.to_string(), None),
            ToolCallDecision::Block(reason) => (raw_args.to_string(), Some(reason)),
            ToolCallDecision::ReplaceArgs(new_args) => (
                serde_json::to_string(&new_args).unwrap_or_else(|_| raw_args.to_string()),
                None,
            ),
        };

        let raw_result: ToolResult = if let Some(reason) = blocked {
            ToolResult::err(format!("Blocked: {reason}"))
        } else {
            match self
                .tools
                .execute(name, &effective_args_str, cancel.clone())
                .await
            {
                Ok(r) => r,
                Err(e) => ToolResult::err(format!("Error: {e}")),
            }
        };

        match self
            .hooks
            .after_tool_call(name, &raw_result, cancel.clone())
            .await
        {
            Some(replaced) => replaced,
            None => raw_result,
        }
    }
}

/// Render a `ToolResult` to the `String` payload that goes into
/// `Message::Tool { content }`. Media references are inlined as `[media: ...]`
/// markers since the OpenAI tool message format only carries a single text
/// field.
fn flatten_for_message(result: ToolResult) -> String {
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

/// One-line projection of a tool's args for the YYC-74 card. Tool-aware
/// for the common tools we ship; falls back to a generic JSON peek so
/// new tools still get something useful before someone writes a
/// custom summarizer for them.
fn summarize_tool_args(name: &str, raw_args: &str) -> Option<String> {
    let args: serde_json::Value = match serde_json::from_str(raw_args) {
        Ok(v) => v,
        Err(_) => return None,
    };
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let tail = |full: String, n: usize| -> String {
        if full.chars().count() <= n {
            full
        } else {
            let chars: Vec<char> = full.chars().collect();
            format!("…{}", chars[chars.len() - n + 1..].iter().collect::<String>())
        }
    };
    let truncate = |full: String, n: usize| -> String {
        if full.chars().count() <= n {
            full
        } else {
            let chars: Vec<char> = full.chars().collect();
            format!("{}…", chars[..n - 1].iter().collect::<String>())
        }
    };
    let summary = match name {
        // File tools — path is the salient bit.
        "read_file" | "write_file" | "edit_file" | "list_files" => {
            s("path").map(|p| tail(p, 60))
        }
        // Search tools — pattern/query.
        "search_files" => s("pattern").map(|p| truncate(p, 60)),
        "code_query" => s("query").map(|q| truncate(q, 60)),
        "code_outline" | "code_extract" => s("path").map(|p| tail(p, 60)),
        "find_symbol" => s("name"),
        // Code semantics — file:line.
        "goto_definition" | "find_references" | "hover" | "diagnostics" => {
            let p = s("path").map(|x| tail(x, 40));
            let line = args.get("line").and_then(|v| v.as_u64());
            match (p, line) {
                (Some(p), Some(l)) => Some(format!("{p}:{l}")),
                (Some(p), None) => Some(p),
                _ => None,
            }
        }
        "rename_symbol" => {
            let p = s("path").map(|x| tail(x, 40));
            let new = s("new_name");
            match (p, new) {
                (Some(p), Some(n)) => Some(format!("{p} → {n}")),
                (Some(p), None) => Some(p),
                _ => None,
            }
        }
        "replace_function_body" => {
            let p = s("path").map(|x| tail(x, 40));
            let sym = s("symbol");
            match (p, sym) {
                (Some(p), Some(sym)) => Some(format!("{p}::{sym}")),
                (Some(p), None) => Some(p),
                _ => None,
            }
        }
        // Web tools.
        "web_search" | "code_search_semantic" => s("query").map(|q| truncate(q, 60)),
        "web_fetch" => s("url").map(|u| truncate(u, 60)),
        // Shell tools.
        "bash" | "run_command" | "pty_create" | "pty_write" => {
            s("command").map(|c| truncate(c, 60))
        }
        "pty_read" | "pty_close" | "pty_resize" => s("session_id").map(|i| truncate(i, 16)),
        "pty_list" => Some("(all sessions)".into()),
        // Git tools.
        "git_status" | "git_log" | "git_diff" => Some(name.to_string()),
        "git_commit" => s("message").map(|m| truncate(m, 60)),
        "git_branch" => {
            let act = s("action").unwrap_or_else(|| "list".into());
            let nm = s("name");
            match nm {
                Some(n) => Some(format!("{act} {n}")),
                None => Some(act),
            }
        }
        "git_push" => {
            let r = s("remote").unwrap_or_else(|| "origin".into());
            let b = s("branch");
            match b {
                Some(b) => Some(format!("{r} {b}")),
                None => Some(r),
            }
        }
        "index_code_graph" | "index_code_embeddings" => Some("(workspace)".into()),
        _ => {
            // Generic: surface the first string-valued field.
            args.as_object().and_then(|o| {
                o.iter()
                    .find_map(|(_, v)| v.as_str().map(|s| truncate(s.to_string(), 60)))
            })
        }
    };
    summary.filter(|s| !s.is_empty())
}

/// One-line metadata about a tool result for the YYC-74 card sub-header
/// (e.g. "847 lines · 26.8 KB", "5 matches", "+12 -3"). Per-tool when
/// the output has structure; falls back to a generic line/char count.
fn summarize_tool_result(name: &str, output: &str) -> Option<String> {
    let text = output.trim();
    if text.is_empty() {
        return None;
    }
    let lines = text.lines().count();
    let bytes = text.len();
    let format_size = |b: usize| -> String {
        if b < 1024 {
            format!("{b} B")
        } else if b < 1024 * 1024 {
            format!("{:.1} KB", (b as f64) / 1024.0)
        } else {
            format!("{:.1} MB", (b as f64) / (1024.0 * 1024.0))
        }
    };
    match name {
        "write_file" => {
            // "Wrote N bytes to PATH"
            let bytes_n = text
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse::<usize>().ok());
            bytes_n.map(|n| format!("{} written", format_size(n)))
        }
        "edit_file" => {
            // "Replaced N occurrence(s) in PATH"
            let n = text
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse::<usize>().ok())?;
            Some(format!("{n} occurrence{}", if n == 1 { "" } else { "s" }))
        }
        "read_file" | "list_files" => {
            Some(format!("{lines} lines · {}", format_size(bytes)))
        }
        "search_files" => {
            // ripgrep output: each non-empty line is a hit
            let hits = text.lines().filter(|l| !l.is_empty()).count();
            Some(format!("{hits} match{}", if hits == 1 { "" } else { "es" }))
        }
        "git_log" => {
            let n = text.lines().filter(|l| !l.is_empty()).count();
            Some(format!("{n} commit{}", if n == 1 { "" } else { "s" }))
        }
        "git_diff" => {
            let plus = text.lines().filter(|l| l.starts_with('+') && !l.starts_with("+++")).count();
            let minus = text.lines().filter(|l| l.starts_with('-') && !l.starts_with("---")).count();
            if plus == 0 && minus == 0 {
                None
            } else {
                Some(format!("+{plus} -{minus}"))
            }
        }
        "git_status" => {
            // First line is "## branch...origin/branch [ahead N]"; rest are file changes.
            let changes = text.lines().filter(|l| !l.starts_with("##") && !l.is_empty()).count();
            if changes == 0 {
                Some("clean".to_string())
            } else {
                Some(format!("{changes} change{}", if changes == 1 { "" } else { "s" }))
            }
        }
        "code_outline" | "find_symbol" => {
            // JSON payloads — peek at the symbol/match counts.
            serde_json::from_str::<serde_json::Value>(text)
                .ok()
                .and_then(|v| {
                    let arr = v.get("symbols").or_else(|| v.get("matches"))?.as_array()?;
                    Some(format!(
                        "{} symbol{}",
                        arr.len(),
                        if arr.len() == 1 { "" } else { "s" }
                    ))
                })
        }
        "code_search_semantic" => serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|v| v.get("matches")?.as_array().map(|a| a.len()))
            .map(|n| format!("{n} hit{}", if n == 1 { "" } else { "s" })),
        "web_search" | "web_fetch" => {
            Some(format!("{lines} lines · {}", format_size(bytes)))
        }
        "bash" | "run_command" => {
            Some(format!("{lines} lines · {}", format_size(bytes)))
        }
        "diagnostics" => serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|v| v.get("count")?.as_u64())
            .map(|n| format!("{n} diagnostic{}", if n == 1 { "" } else { "s" })),
        // Generic fallback so even an unknown tool gets *something*.
        _ => {
            if lines == 1 {
                Some(format_size(bytes))
            } else {
                Some(format!("{lines} lines · {}", format_size(bytes)))
            }
        }
    }
}

/// Truncated tool result for the YYC-74 card preview block — caps at
/// ~12 lines / 1 KB. The full output still goes to the LLM via
/// `Message::Tool`; this is purely for rendering.
fn preview_output(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let chars: Vec<char> = trimmed.chars().take(1024).collect();
    let head: String = chars.iter().collect();
    let lines: Vec<&str> = head.lines().take(12).collect();
    Some(lines.join("\n"))
}

/// Number of full output lines hidden by `preview_output` (YYC-78).
/// Used by the card to render `… N more lines elided` when the
/// result was clipped.
fn elided_lines(text: &str, preview: Option<&str>) -> usize {
    let total = text.trim().lines().count();
    let shown = preview.map(|p| p.lines().count()).unwrap_or(0);
    total.saturating_sub(shown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::HookRegistry;
    use crate::provider::mock::{MockProvider, MockResponse};
    use crate::skills::SkillRegistry;
    use crate::tools::ToolRegistry;
    use std::sync::Arc;

    fn empty_skills() -> Arc<SkillRegistry> {
        // Point at a path that doesn't exist so the registry is empty.
        Arc::new(SkillRegistry::new(&std::path::PathBuf::from(
            "/tmp/vulcan-test-skills-nonexistent",
        )))
    }

    /// Build an Agent with a MockProvider and minimal setup. Returns the agent
    /// and a handle to the mock so tests can enqueue responses + inspect calls.
    fn agent_with_mock() -> (Agent, Arc<MockProvider>) {
        let mock = Arc::new(MockProvider::new(128_000));
        // The agent needs Box<dyn LLMProvider>; we wrap a clone of the Arc.
        // Since MockProvider's state is in interior Mutex, cloning the Arc
        // gives the test a handle to the same instance.
        struct ProviderHandle(Arc<MockProvider>);
        #[async_trait::async_trait]
        impl LLMProvider for ProviderHandle {
            async fn chat(
                &self,
                m: &[Message],
                t: &[crate::provider::ToolDefinition],
                c: CancellationToken,
            ) -> Result<crate::provider::ChatResponse> {
                self.0.chat(m, t, c).await
            }
            async fn chat_stream(
                &self,
                m: &[Message],
                t: &[crate::provider::ToolDefinition],
                tx: tokio::sync::mpsc::UnboundedSender<crate::provider::StreamEvent>,
                c: CancellationToken,
            ) -> Result<()> {
                self.0.chat_stream(m, t, tx, c).await
            }
            fn max_context(&self) -> usize {
                self.0.max_context()
            }
        }
        let agent = Agent::for_test(
            Box::new(ProviderHandle(mock.clone())),
            ToolRegistry::new(),
            HookRegistry::new(),
            empty_skills(),
        );
        (agent, mock)
    }

    #[tokio::test]
    async fn single_turn_text_response() {
        let (mut agent, mock) = agent_with_mock();
        mock.enqueue_text("Hello there");

        let resp = agent.run_prompt("hi").await.unwrap();
        assert_eq!(resp, "Hello there");

        // Provider was called once; messages had system + user (no history).
        let calls = mock.captured_calls();
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0][0], Message::System { .. }));
        match &calls[0][1] {
            Message::User { content } => assert_eq!(content, "hi"),
            other => panic!("expected User, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn multi_turn_with_tool_call() {
        let (mut agent, mock) = agent_with_mock();
        // Iter 0: tool call. Iter 1: final text response.
        mock.enqueue_tool_call(
            "read_file",
            "call_1",
            serde_json::json!({"path": "/tmp/vulcan-test-nonexistent-file"}),
        );
        mock.enqueue_text("Read failed but that's fine for the test");

        // The real ReadFile tool is registered by ToolRegistry::new(); it'll
        // return Err for the bogus path, which dispatch_tool wraps as
        // ToolResult::err. The agent's iteration 1 sees a Tool message with
        // the error string and emits the queued text response.
        let resp = agent.run_prompt("read it").await.unwrap();
        assert_eq!(resp, "Read failed but that's fine for the test");

        let calls = mock.captured_calls();
        assert_eq!(
            calls.len(),
            2,
            "should call provider twice (tool, then final)"
        );

        // Iteration 1's messages should include the tool result.
        let iter1 = &calls[1];
        assert!(
            iter1.iter().any(|m| matches!(m, Message::Tool { .. })),
            "iteration 1 should include a Tool message in history"
        );
    }

    #[tokio::test]
    async fn streaming_and_buffered_paths_match() {
        // Same scripted response in both paths; final returned text should match.
        let (mut a1, m1) = agent_with_mock();
        m1.enqueue_text("identical output");
        let buffered = a1.run_prompt("x").await.unwrap();

        let (mut a2, m2) = agent_with_mock();
        m2.enqueue_text("identical output");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let streamed = a2.run_prompt_stream("x", tx).await.unwrap();
        // Drain the channel.
        while let Ok(_) = rx.try_recv() {}

        assert_eq!(buffered, streamed);
        assert_eq!(buffered, "identical output");
    }

    #[tokio::test]
    async fn provider_error_propagates() {
        let (mut agent, mock) = agent_with_mock();
        mock.enqueue_error("simulated 500");

        let result = agent.run_prompt("anything").await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("simulated 500"), "got {msg:?}");
    }

    #[tokio::test]
    async fn reasoning_carries_into_assistant_message() {
        let (mut agent, mock) = agent_with_mock();
        mock.enqueue(MockResponse::WithReasoning {
            reasoning: "the user wants a greeting".into(),
            content: "Hi!".into(),
        });

        let resp = agent.run_prompt("hello").await.unwrap();
        assert_eq!(resp, "Hi!");
        // run_prompt's final save_messages would have stored the reasoning;
        // not asserting against the DB to avoid touching ~/.vulcan in tests.
    }

    #[tokio::test]
    async fn fork_session_records_lineage_and_switches_active_session() {
        let (mut agent, _mock) = agent_with_mock();
        let parent_id = agent.session_id().to_string();

        let child_id = agent.fork_session(Some("branched for UI work")).unwrap();

        assert_eq!(agent.session_id(), child_id);
        let summaries = agent.memory().list_sessions(10).unwrap();
        let child = summaries
            .iter()
            .find(|s| s.id == child_id)
            .expect("child summary should exist");
        assert_eq!(child.parent_session_id.as_deref(), Some(parent_id.as_str()));
        assert_eq!(child.lineage_label.as_deref(), Some("branched for UI work"));
    }

    /// Tool that increments an in-flight counter, sleeps, then decrements.
    /// Records the maximum observed concurrency so the test can assert that
    /// parallel dispatch actually overlaps tool execution (YYC-34).
    struct ConcurrencyProbeTool {
        in_flight: Arc<std::sync::atomic::AtomicUsize>,
        max_observed: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl crate::tools::Tool for ConcurrencyProbeTool {
        fn name(&self) -> &str {
            "concurrency_probe"
        }
        fn description(&self) -> &str {
            "test tool that sleeps and tracks in-flight concurrency"
        }
        fn schema(&self) -> Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        async fn call(
            &self,
            _params: Value,
            _cancel: CancellationToken,
        ) -> Result<crate::tools::ToolResult> {
            use std::sync::atomic::Ordering;
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_observed.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(crate::tools::ToolResult::ok("done"))
        }
    }

    #[tokio::test]
    async fn parallel_tool_calls_dispatch_concurrently() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_observed = Arc::new(AtomicUsize::new(0));

        let mut tools = ToolRegistry::new();
        tools.register(Arc::new(ConcurrencyProbeTool {
            in_flight: in_flight.clone(),
            max_observed: max_observed.clone(),
        }));

        let mock = Arc::new(MockProvider::new(128_000));
        struct ProviderHandle(Arc<MockProvider>);
        #[async_trait::async_trait]
        impl LLMProvider for ProviderHandle {
            async fn chat(
                &self,
                m: &[Message],
                t: &[crate::provider::ToolDefinition],
                c: CancellationToken,
            ) -> Result<crate::provider::ChatResponse> {
                self.0.chat(m, t, c).await
            }
            async fn chat_stream(
                &self,
                m: &[Message],
                t: &[crate::provider::ToolDefinition],
                tx: tokio::sync::mpsc::UnboundedSender<crate::provider::StreamEvent>,
                c: CancellationToken,
            ) -> Result<()> {
                self.0.chat_stream(m, t, tx, c).await
            }
            fn max_context(&self) -> usize {
                self.0.max_context()
            }
        }
        let mut agent = Agent::for_test(
            Box::new(ProviderHandle(mock.clone())),
            tools,
            HookRegistry::new(),
            empty_skills(),
        );

        // Iter 0: three parallel calls. Iter 1: final text.
        mock.enqueue_tool_calls(vec![
            ("concurrency_probe", "call_a", serde_json::json!({})),
            ("concurrency_probe", "call_b", serde_json::json!({})),
            ("concurrency_probe", "call_c", serde_json::json!({})),
        ]);
        mock.enqueue_text("done");

        let started = std::time::Instant::now();
        let resp = agent.run_prompt("go").await.unwrap();
        let elapsed = started.elapsed();

        assert_eq!(resp, "done");
        // Three sequential 40ms sleeps would be ~120ms; parallel ≈ 40ms.
        // Allow generous slack for runtime jitter.
        assert!(
            elapsed < std::time::Duration::from_millis(110),
            "dispatch took {elapsed:?} — looks sequential"
        );
        assert!(
            max_observed.load(Ordering::SeqCst) >= 2,
            "expected ≥2 concurrent dispatches, observed {}",
            max_observed.load(Ordering::SeqCst)
        );

        // Order preservation: tool messages line up with original call ids.
        let calls = mock.captured_calls();
        let iter1 = &calls[1];
        let tool_ids: Vec<&str> = iter1
            .iter()
            .filter_map(|m| match m {
                Message::Tool { tool_call_id, .. } => Some(tool_call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(tool_ids, vec!["call_a", "call_b", "call_c"]);
    }

    #[test]
    fn summarize_tool_args_picks_meaningful_field_per_tool() {
        // YYC-74 — the YYC-74 card needs a one-line projection.
        assert_eq!(
            summarize_tool_args("read_file", r#"{"path":"src/foo.rs"}"#).as_deref(),
            Some("src/foo.rs")
        );
        assert_eq!(
            summarize_tool_args("git_commit", r#"{"message":"YYC-74"}"#).as_deref(),
            Some("YYC-74")
        );
        assert_eq!(
            summarize_tool_args("git_branch", r#"{"action":"create","name":"foo"}"#)
                .as_deref(),
            Some("create foo")
        );
        // Long path tail-truncates rather than head-truncates.
        let long_path = "/very/long/leading/path/segments/that/blow/the/budget/file.rs";
        let result =
            summarize_tool_args("read_file", &format!(r#"{{"path":"{long_path}"}}"#))
                .unwrap();
        assert!(result.starts_with('…'));
        assert!(result.ends_with("file.rs"), "got {result}");
        // Generic fallback for unknown tools surfaces first string field.
        assert_eq!(
            summarize_tool_args("custom_tool", r#"{"x":42,"label":"hello"}"#).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn preview_output_caps_to_twelve_lines_and_one_kb() {
        // YYC-78 raised the cap so collapsed cards still show useful
        // context up front.
        let big = (1..=40).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
        let preview = preview_output(&big).unwrap();
        assert_eq!(preview.lines().count(), 12);
        assert!(preview.contains("line 1"));
        assert!(!preview.contains("line 13"));
    }

    #[test]
    fn elided_lines_counts_what_was_clipped() {
        let big = (1..=40).map(|n| format!("line {n}")).collect::<Vec<_>>().join("\n");
        let preview = preview_output(&big);
        let elided = elided_lines(&big, preview.as_deref());
        assert_eq!(elided, 28);
        // Short output → no elision.
        let short = "one\ntwo\nthree";
        let preview = preview_output(short);
        assert_eq!(elided_lines(short, preview.as_deref()), 0);
    }

    #[test]
    fn preview_output_returns_none_for_empty() {
        assert!(preview_output("").is_none());
        assert!(preview_output("   \n  ").is_none());
    }

    #[test]
    fn summarize_tool_result_per_tool_meta() {
        // YYC-74: meta sub-header in the card.
        assert_eq!(
            summarize_tool_result("write_file", "Wrote 4321 bytes to /tmp/x").as_deref(),
            Some("4.2 KB written")
        );
        assert_eq!(
            summarize_tool_result("edit_file", "Replaced 3 occurrence(s) in /tmp/x").as_deref(),
            Some("3 occurrences")
        );
        assert_eq!(
            summarize_tool_result("git_status", "## main\n M src/foo.rs\n?? new.rs").as_deref(),
            Some("2 changes")
        );
        assert_eq!(summarize_tool_result("git_status", "## main").as_deref(), Some("clean"));
        assert_eq!(
            summarize_tool_result("git_diff", "+++ a\n+ added\n--- b\n- removed\n- removed2")
                .as_deref(),
            Some("+1 -2")
        );
        // Generic fallback (unknown tool) gets line/byte count.
        let s =
            summarize_tool_result("unknown_tool", "line one\nline two\nline three").unwrap();
        assert!(s.starts_with("3 lines"), "got {s}");
    }
}
