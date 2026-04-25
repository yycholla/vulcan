use std::sync::Arc;

use crate::config::Config;
use crate::context::ContextManager;
use crate::hooks::skills::SkillsHook;
use crate::hooks::{HookRegistry, ToolCallDecision};
use crate::memory::SessionStore;
use crate::prompt_builder::PromptBuilder;
use crate::provider::openai::OpenAIProvider;
use crate::provider::{LLMProvider, Message, StreamEvent};
use crate::skills::SkillRegistry;
use crate::tools::{ToolRegistry, ToolResult};
use anyhow::Result;
use serde_json::Value;
use tokio::sync::mpsc;
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
}

impl Agent {
    /// Construct an Agent with no caller-supplied hooks. Built-in hooks (skills
    /// injection, etc.) are still registered.
    pub fn new(config: &Config) -> Self {
        Self::with_hooks(config, HookRegistry::new())
    }

    /// Construct an Agent with a caller-supplied hook registry. Built-in hooks
    /// (currently: skills) are registered into it before it's wrapped in Arc.
    /// Fires `session_start` is up to the caller — see `start_session`.
    pub fn with_hooks(config: &Config, mut hooks: HookRegistry) -> Self {
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
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
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
        let system = self.prompt_builder.build_system_prompt(&self.tools);
        let tool_defs = self.tools.definitions();
        let mut messages = vec![Message::System { content: system }];

        if let Some(session_id) = self.memory.last_session_id() {
            if let Some(history) = self.memory.load_history(&session_id)? {
                for msg in history {
                    messages.push(msg);
                }
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

            // ── BeforePrompt: handlers may inject extra messages. Injections
            // are transient — they go on the wire but don't persist into the
            // conversation history we save to memory.
            let outgoing = self.hooks.apply_before_prompt(&messages).await;

            let response = self.provider.chat(&outgoing, &tool_defs).await?;

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
                    let final_result =
                        self.dispatch_tool(&tc.function.name, &tc.function.arguments).await;
                    messages.push(Message::Tool {
                        tool_call_id: tc.id.clone(),
                        content: final_result,
                    });
                }
            } else {
                let text = response.content.unwrap_or_default();

                // ── BeforeAgentEnd: a handler may force the loop to continue.
                if let Some(instruction) = self.hooks.before_agent_end(&text).await {
                    messages.push(Message::Assistant {
                        content: Some(text.clone()),
                        tool_calls: None,
                    });
                    messages.push(Message::User { content: instruction });
                    continue;
                }

                self.memory.save_messages(&messages)?;
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
        let system = self.prompt_builder.build_system_prompt(&self.tools);
        let tool_defs = self.tools.definitions();
        let mut messages = vec![Message::System { content: system }];

        if let Some(session_id) = self.memory.last_session_id() {
            if let Some(history) = self.memory.load_history(&session_id)? {
                for msg in history {
                    messages.push(msg);
                }
            }
        }

        messages.push(Message::User {
            content: input.to_string(),
        });

        let mut full_response = String::new();

        for iteration in 0..10 {
            // ── BeforePrompt (transient — see run_prompt for rationale).
            let outgoing = self.hooks.apply_before_prompt(&messages).await;

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
                .chat_stream(&outgoing, &tool_defs, inner_tx)
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
                    let final_result =
                        self.dispatch_tool(&tc.function.name, &tc.function.arguments).await;
                    messages.push(Message::Tool {
                        tool_call_id: tc.id.clone(),
                        content: final_result,
                    });
                }
            } else {
                // ── BeforeAgentEnd
                if let Some(instruction) = self.hooks.before_agent_end(&full_response).await {
                    messages.push(Message::Assistant {
                        content: Some(full_response.clone()),
                        tool_calls: None,
                    });
                    messages.push(Message::User { content: instruction });
                    continue;
                }

                self.memory.save_messages(&messages)?;
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

    /// Resume a previous session by ID
    pub async fn resume_session(&mut self, session_id: &str) -> Result<()> {
        let history = self.memory.load_history(session_id)?.unwrap_or_default();

        if history.is_empty() {
            eprintln!("No session found with ID: {session_id}");
            return Ok(());
        }

        println!(
            "Resumed session {session_id} ({} messages)",
            history.len()
        );
        Ok(())
    }

    /// Dispatch a single tool call, running BeforeToolCall + AfterToolCall
    /// hooks around it. Returns the result flattened to the `String` payload
    /// expected by `Message::Tool` (media references inlined as `[media: ...]`
    /// markers). Hooks see the full `ToolResult`.
    async fn dispatch_tool(&self, name: &str, raw_args: &str) -> String {
        let parsed_args: Value = serde_json::from_str(raw_args).unwrap_or(Value::Null);

        let (effective_args_str, blocked) =
            match self.hooks.before_tool_call(name, &parsed_args).await {
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
            match self.tools.execute(name, &effective_args_str).await {
                Ok(r) => r,
                Err(e) => ToolResult::err(format!("Error: {e}")),
            }
        };

        let final_result = match self.hooks.after_tool_call(name, &raw_result).await {
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
