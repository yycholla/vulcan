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
use anyhow::Result;
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
    pub fn new(config: &Config) -> Self {
        Self::with_hooks_and_pause(config, HookRegistry::new(), None)
    }

    /// Construct with caller-supplied hooks and no interactive pause channel.
    /// The TUI uses `with_hooks_and_pause` to wire one up.
    pub fn with_hooks(config: &Config, hooks: HookRegistry) -> Self {
        Self::with_hooks_and_pause(config, hooks, None)
    }

    /// Construct an Agent with a caller-supplied hook registry and an optional
    /// pause emitter. Built-ins (skills, safety) are registered into the
    /// registry; if a pause emitter is provided, `SafetyHook` is wired up to
    /// route blocks through it as `AgentPause::SafetyApproval`.
    pub fn with_hooks_and_pause(
        config: &Config,
        mut hooks: HookRegistry,
        pause_tx: Option<PauseSender>,
    ) -> Self {
        let api_key = config
            .api_key()
            .expect("No API key configured. Set VULCAN_API_KEY or add api_key to config.toml");

        let provider: Box<dyn LLMProvider> = Box::new(
            OpenAIProvider::new(
                &config.provider.base_url,
                &api_key,
                &config.provider.model,
                config.provider.max_context,
            )
            .expect("Failed to initialize LLM provider"),
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

        Self {
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
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
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
            let outgoing = self.hooks.apply_before_prompt(&messages, cancel.clone()).await;

            let response = self.provider.chat(&outgoing, &tool_defs, cancel.clone()).await?;

            if let Some(usage) = &response.usage {
                self.context
                    .record_usage(usage.prompt_tokens, usage.completion_tokens);
            }

            if let Some(tool_calls) = &response.tool_calls {
                messages.push(Message::Assistant {
                    content: response.content.clone(),
                    tool_calls: Some(tool_calls.clone()),
                });

                for tc in tool_calls {
                    tracing::info!("Executing tool: {} (call {})", tc.function.name, tc.id);
                    let final_result = self
                        .dispatch_tool(&tc.function.name, &tc.function.arguments, cancel.clone())
                        .await;
                    messages.push(Message::Tool {
                        tool_call_id: tc.id.clone(),
                        content: final_result,
                    });
                }
            } else {
                let text = response.content.unwrap_or_default();

                // ── BeforeAgentEnd: a handler may force the loop to continue.
                if let Some(instruction) = self.hooks.before_agent_end(&text, cancel.clone()).await {
                    messages.push(Message::Assistant {
                        content: Some(text.clone()),
                        tool_calls: None,
                    });
                    messages.push(Message::User { content: instruction });
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
                }));
                return Ok("Cancelled".to_string());
            }

            // ── BeforePrompt (transient — see run_prompt for rationale).
            let outgoing = self.hooks.apply_before_prompt(&messages, cancel.clone()).await;

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

            self.provider
                .chat_stream(&outgoing, &tool_defs, inner_tx, cancel.clone())
                .await?;

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
                None => return Err(anyhow::anyhow!("Stream ended without Done event")),
            };

            if let Some(text) = &response.content {
                full_response.push_str(text);
            }

            if let Some(tool_calls) = &response.tool_calls {
                messages.push(Message::Assistant {
                    content: response.content.clone(),
                    tool_calls: Some(tool_calls.clone()),
                });

                for tc in tool_calls {
                    tracing::info!("Executing tool: {} (call {})", tc.function.name, tc.id);
                    let final_result = self
                        .dispatch_tool(&tc.function.name, &tc.function.arguments, cancel.clone())
                        .await;
                    messages.push(Message::Tool {
                        tool_call_id: tc.id.clone(),
                        content: final_result,
                    });
                }
            } else {
                // ── BeforeAgentEnd
                if let Some(instruction) = self.hooks.before_agent_end(&full_response, cancel.clone()).await {
                    messages.push(Message::Assistant {
                        content: Some(full_response.clone()),
                        tool_calls: None,
                    });
                    messages.push(Message::User { content: instruction });
                    continue;
                }

                self.memory.save_messages(&self.session_id, &messages)?;
                self.turns = self.turns.saturating_add(1);
                if iteration >= 5 {
                    self.skills.try_auto_create(input, &full_response)?;
                }
                let _ = ui_tx.send(StreamEvent::Done(crate::provider::ChatResponse {
                    content: Some(full_response.clone()),
                    tool_calls: None,
                    usage: response.usage,
                    finish_reason: response.finish_reason,
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
        tracing::info!(
            "resumed session {session_id} ({} messages)",
            history.len()
        );
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
    async fn dispatch_tool(
        &self,
        name: &str,
        raw_args: &str,
        cancel: CancellationToken,
    ) -> String {
        let parsed_args: Value = serde_json::from_str(raw_args).unwrap_or(Value::Null);

        let (effective_args_str, blocked) =
            match self.hooks.before_tool_call(name, &parsed_args, cancel.clone()).await {
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
            match self.tools.execute(name, &effective_args_str, cancel.clone()).await {
                Ok(r) => r,
                Err(e) => ToolResult::err(format!("Error: {e}")),
            }
        };

        let final_result = match self.hooks.after_tool_call(name, &raw_result, cancel.clone()).await {
            Some(replaced) => replaced,
            None => raw_result,
        };

        flatten_for_message(final_result)
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
