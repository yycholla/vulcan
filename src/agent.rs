use std::sync::Arc;

use crate::config::Config;
use crate::context::ContextManager;
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
        if !config.provider.disable_catalog {
            match Self::fetch_catalog(config, &api_key).await {
                Ok(models) => {
                    let found = models.iter().find(|m| m.id == config.provider.model);
                    match found {
                        Some(model_info) => {
                            supports_json_mode = model_info.features.json_mode;
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

        let tools = ToolRegistry::new();
        let skills = Arc::new(SkillRegistry::new(&config.skills_dir));
        let memory = SessionStore::new();
        let context = ContextManager::new(provider.max_context());
        let session_id = Uuid::new_v4().to_string();

        // Built-in hook: surface available skills to the LLM via BeforePrompt.
        hooks.register(Arc::new(SkillsHook::new(skills.clone())));

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
        })
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
            memory: SessionStore::new(),
            prompt_builder: PromptBuilder,
            hooks: Arc::new(hooks),
            session_id: Uuid::new_v4().to_string(),
            turns: 0,
            turn_cancel: CancellationToken::new(),
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
        self.hooks.session_start(&self.session_id).await;
    }

    /// Fires `session_end` and records the total turn count.
    pub async fn end_session(&self) {
        self.hooks.session_end(&self.session_id, self.turns).await;
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

        for iteration in 0..10 {
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

                for tc in tool_calls {
                    tracing::info!("Executing tool: {} (call {})", tc.function.name, tc.id);
                    let result = self
                        .dispatch_tool(&tc.function.name, &tc.function.arguments, cancel.clone())
                        .await;
                    messages.push(Message::Tool {
                        tool_call_id: tc.id.clone(),
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

                self.memory.save_messages(&self.session_id, &messages)?;
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

        for iteration in 0..10 {
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

                for tc in tool_calls {
                    tracing::info!("Executing tool: {} (call {})", tc.function.name, tc.id);
                    // Surface tool activity to the TUI so the chat doesn't sit
                    // on "Thinking…" while the tool runs (YYC-57).
                    let _ = ui_tx.send(StreamEvent::ToolCallStart {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                    });
                    let result = self
                        .dispatch_tool(&tc.function.name, &tc.function.arguments, cancel.clone())
                        .await;
                    let ok = !result.is_error;
                    let _ = ui_tx.send(StreamEvent::ToolCallEnd {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        ok,
                    });
                    messages.push(Message::Tool {
                        tool_call_id: tc.id.clone(),
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

                self.memory.save_messages(&self.session_id, &messages)?;
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

    /// Borrow the underlying `SessionStore`. Used by the TUI's `/search`
    /// command and the `vulcan search` CLI subcommand to run FTS queries.
    pub fn memory(&self) -> &crate::memory::SessionStore {
        &self.memory
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

}
