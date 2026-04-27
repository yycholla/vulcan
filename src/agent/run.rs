use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::provider::{Message, StreamEvent, Usage};

use super::Agent;
use super::dispatch::{elided_lines, preview_output, summarize_tool_args, summarize_tool_result};
use super::flatten_for_message;

/// Threshold for "near context limit" hint on empty terminal turns.
/// Picked low enough that a llama-cpp Q2_0 model that's about to drop
/// history still triggers it before the loop exits silently (YYC-104).
const NEAR_CONTEXT_LIMIT_RATIO: f64 = 0.95;

/// Compose the user-facing message for an empty terminal turn (YYC-104).
/// Replaces the bare `_(model returned empty response)_` placeholder so the
/// user can tell *why* the loop ended: how many tools ran, whether the
/// model emitted a reasoning trace that got stripped, and whether the
/// prompt was up against the context window.
pub(in crate::agent) fn empty_terminal_message(
    iteration: usize,
    tool_calls_total: usize,
    reasoning_len: usize,
    usage: Option<&Usage>,
    max_context: usize,
) -> String {
    let mut parts = vec![format!(
        "model returned empty response after {} tool call{} on iteration {}",
        tool_calls_total,
        if tool_calls_total == 1 { "" } else { "s" },
        iteration,
    )];
    if reasoning_len > 0 {
        parts.push(format!("reasoning trace was {reasoning_len} chars"));
    }
    if let Some(u) = usage {
        if max_context > 0
            && (u.prompt_tokens as f64) >= NEAR_CONTEXT_LIMIT_RATIO * (max_context as f64)
        {
            parts.push(format!(
                "prompt at {}/{} tokens (near context limit)",
                u.prompt_tokens, max_context
            ));
        }
    }
    format!("_({} — terminal turn)_", parts.join("; "))
}

impl Agent {
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
        // YYC-104: count tool calls across iterations so the empty-terminal
        // message can tell the user how far the agent got before stalling.
        let mut tool_calls_total: usize = 0;
        let mut last_usage: Option<Usage> = None;
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
                last_usage = Some(usage.clone());
            }

            if let Some(tool_calls) = &response.tool_calls {
                tool_calls_total = tool_calls_total.saturating_add(tool_calls.len());
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
                let mut text = response.content.unwrap_or_default();
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

                // YYC-104: empty terminal turns previously returned an empty
                // string, so callers (CLI prompt path, gateway lanes) saw the
                // turn end silently. Surface a structured hint instead.
                if text.is_empty() {
                    let reasoning_len = reasoning.as_deref().map(str::len).unwrap_or(0);
                    let max_ctx = self.provider.max_context();
                    let hint = empty_terminal_message(
                        iteration,
                        tool_calls_total,
                        reasoning_len,
                        last_usage.as_ref(),
                        max_ctx,
                    );
                    tracing::warn!(
                        iteration,
                        tool_calls_total,
                        reasoning_len,
                        prompt_tokens =
                            last_usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                        max_context = max_ctx,
                        "agent: model returned empty content with no tool calls",
                    );
                    text = hint;
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
        // YYC-104: track tool calls + last usage across iterations so the
        // empty-terminal-turn message can describe what happened.
        let mut tool_calls_total: usize = 0;
        let mut last_usage: Option<Usage> = None;
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

            if let Some(usage) = &response.usage {
                last_usage = Some(usage.clone());
            }

            if let Some(tool_calls) = &response.tool_calls {
                tool_calls_total = tool_calls_total.saturating_add(tool_calls.len());
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
                        let preview_source =
                            result.display_preview.as_deref().unwrap_or(&result.output);
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

                // YYC-104: empty terminal turn — surface a structured hint
                // (iteration, tool count, reasoning length, context-limit
                // status) so the user can tell *why* the loop ended rather
                // than seeing a bare placeholder.
                if full_response.is_empty() {
                    let reasoning_len = reasoning.as_deref().map(str::len).unwrap_or(0);
                    let max_ctx = self.provider.max_context();
                    let hint = empty_terminal_message(
                        iteration,
                        tool_calls_total,
                        reasoning_len,
                        last_usage.as_ref(),
                        max_ctx,
                    );
                    tracing::warn!(
                        iteration,
                        tool_calls_total,
                        reasoning_len,
                        prompt_tokens =
                            last_usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                        max_context = max_ctx,
                        "agent: model returned empty content with no tool calls",
                    );
                    full_response = hint.clone();
                    let _ = ui_tx.send(StreamEvent::Text(hint));
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
}
