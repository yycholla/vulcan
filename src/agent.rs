use crate::config::Config;
use crate::context::ContextManager;
use crate::memory::SessionStore;
use crate::prompt_builder::PromptBuilder;
use crate::provider::openai::OpenAIProvider;
use crate::provider::{LLMProvider, Message, StreamEvent};
use crate::skills::SkillRegistry;
use crate::tools::ToolRegistry;
use anyhow::Result;
use tokio::sync::mpsc;

/// The core agent — orchestrates the LLM, tools, and state
pub struct Agent {
    provider: Box<dyn LLMProvider>,
    tools: ToolRegistry,
    skills: SkillRegistry,
    context: ContextManager,
    memory: SessionStore,
    prompt_builder: PromptBuilder,
}

impl Agent {
    pub fn new(config: &Config) -> Self {
        let api_key = config
            .api_key()
            .expect("No API key configured. Set FERRIS_API_KEY or add api_key to config.toml");

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
        let skills = SkillRegistry::new(&config.skills_dir);
        let memory = SessionStore::new();
        let context = ContextManager::new(provider.max_context());

        Self {
            provider,
            tools,
            skills,
            context,
            memory,
            prompt_builder: PromptBuilder,
        }
    }

    /// Run a one-shot prompt (no TUI). Gathers context, calls LLM, dispatches tools, returns result.
    pub async fn run_prompt(&mut self, input: &str) -> Result<String> {
        let system = self
            .prompt_builder
            .build_system_prompt(&self.skills, &self.tools);
        let tool_defs = self.tools.definitions();
        let mut messages = vec![Message::System { content: system }];

        // Load recent session context for continuity
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

        // Main agent loop — up to 10 iterations to prevent runaway tool chains
        for iteration in 0..10 {
            tracing::debug!("Agent iteration {iteration}");

            // Check if we need to compact context
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

            let response = self.provider.chat(&messages, &tool_defs).await?;

            let has_content = response.content.as_deref().unwrap_or("").len();
            let has_tools = response.tool_calls.as_ref().map_or(0, |t| t.len());
            tracing::debug!(
                "LLM response: content={} chars, tool_calls={}, finish={:?}",
                has_content,
                has_tools,
                response.finish_reason,
            );

            // Track token usage
            if let Some(usage) = &response.usage {
                self.context
                    .record_usage(usage.prompt_tokens, usage.completion_tokens);
            }

            // If there are tool calls, execute them
            if let Some(tool_calls) = &response.tool_calls {
                messages.push(Message::Assistant {
                    content: response.content.clone(),
                    tool_calls: Some(tool_calls.clone()),
                });

                for tc in tool_calls {
                    tracing::info!("Executing tool: {} (call {})", tc.function.name, tc.id);

                    let result = self
                        .tools
                        .execute(&tc.function.name, &tc.function.arguments)
                        .await;

                    let output = match &result {
                        Ok(o) => o.clone(),
                        Err(e) => format!("Error: {e}"),
                    };

                    messages.push(Message::Tool {
                        tool_call_id: tc.id.clone(),
                        content: output,
                    });
                }
            } else {
                // No tool calls — this is the final answer
                let text = response.content.unwrap_or_default();

                // Save to session history
                self.memory.save_messages(&messages)?;

                // Try to auto-create a skill if this was a complex task
                if iteration >= 5 {
                    self.skills.try_auto_create(input, &text)?;
                }

                return Ok(text);
            }
        }

        Ok("Agent reached maximum iteration limit.".to_string())
    }

    /// Run a prompt with streaming — sends text tokens through `ui_tx` as they arrive.
    /// After the LLM finishes, tool calls are executed and results returned inline.
    pub async fn run_prompt_stream(
        &mut self,
        input: &str,
        ui_tx: mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<String> {
        let system = self
            .prompt_builder
            .build_system_prompt(&self.skills, &self.tools);
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
            // Channel 1: provider writes stream events here
            let (inner_tx, mut inner_rx) = mpsc::unbounded_channel::<StreamEvent>();
            // Channel 2: agent reads Done/Error from here
            let (priv_tx, mut priv_rx) = mpsc::unbounded_channel::<StreamEvent>();

            // Fork task: forward Text to UI, forward Done/Error to agent's private channel
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

            // Start the provider stream (writes to inner_tx)
            self.provider
                .chat_stream(&messages, &tool_defs, inner_tx)
                .await?;

            // Wait for Done from the private channel
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

            tracing::debug!(
                "LLM response: content={} chars, tool_calls={}",
                response.content.as_deref().unwrap_or("").len(),
                response.tool_calls.as_ref().map_or(0, |t| t.len()),
            );

            if let Some(text) = &response.content {
                full_response.push_str(text);
            }

            // Handle tool calls
            if let Some(tool_calls) = &response.tool_calls {
                messages.push(Message::Assistant {
                    content: response.content.clone(),
                    tool_calls: Some(tool_calls.clone()),
                });

                for tc in tool_calls {
                    tracing::info!("Executing tool: {} (call {})", tc.function.name, tc.id);
                    let result = self
                        .tools
                        .execute(&tc.function.name, &tc.function.arguments)
                        .await;
                    let output = match &result {
                        Ok(o) => o.clone(),
                        Err(e) => format!("Error: {e}"),
                    };
                    messages.push(Message::Tool {
                        tool_call_id: tc.id.clone(),
                        content: output,
                    });
                }
                // Loop back for next LLM call with tool results
            } else {
                // Final answer
                self.memory.save_messages(&messages)?;
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
        let history = self
            .memory
            .load_history(session_id)?
            .unwrap_or_default();

        if history.is_empty() {
            eprintln!("No session found with ID: {session_id}");
            return Ok(());
        }

        println!(
            "Resumed session {session_id} ({} messages)",
            history.len()
        );
        // In TUI mode this would rehydrate the conversation
        Ok(())
    }
}
