//! Streaming + buffered prompt execution extracted from `agent/mod.rs`
//! (YYC-109 redo). Owns `run_prompt`, `run_prompt_stream(_with_cancel)`,
//! the streaming pipeline helpers (prepare/compact/collect/execute), the
//! orphan-tool sanitizer, and the empty-terminal hint.

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::provider::{ChatResponse, Message, StreamEvent, ToolCall, ToolDefinition, Usage};
use crate::tools::ToolResult;

use super::dispatch::{elided_lines, preview_output, summarize_tool_args, summarize_tool_result};
use super::{Agent, StreamTurn, flatten_for_message};

/// Threshold for "near context limit" hint on empty terminal turns.
/// Picked low enough that a llama-cpp Q2_0 model that's about to drop
/// history still triggers it before the loop exits silently (YYC-104).
const NEAR_CONTEXT_LIMIT_RATIO: f64 = 0.95;

/// Compose the user-facing message for an empty terminal turn (YYC-104).
/// Replaces the bare `_(model returned empty response)_` placeholder so the
/// user can tell *why* the loop ended: how many tools ran, whether the
/// model emitted a reasoning trace that got stripped, and whether the
/// prompt was up against the context window.
fn empty_terminal_message(
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
    if let Some(u) = usage
        && max_context > 0
        && (u.prompt_tokens as f64) >= NEAR_CONTEXT_LIMIT_RATIO * (max_context as f64)
    {
        parts.push(format!(
            "prompt at {}/{} tokens (near context limit)",
            u.prompt_tokens, max_context
        ));
    }
    format!("_({} — terminal turn)_", parts.join("; "))
}
pub(crate) fn sanitize_orphan_tool_messages(messages: &mut Vec<Message>) -> usize {
    use std::collections::HashSet;
    let mut active_call_ids: HashSet<String> = HashSet::new();
    let mut drop_indices: HashSet<usize> = HashSet::new();
    for (idx, msg) in messages.iter().enumerate() {
        match msg {
            Message::Assistant {
                tool_calls: Some(tcs),
                ..
            } if !tcs.is_empty() => {
                active_call_ids = tcs.iter().map(|tc| tc.id.clone()).collect();
            }
            Message::Assistant { .. } => {
                // No tool_calls (or an empty vec — same on the wire).
                // Any Tool message after this is an orphan until the
                // next Assistant turn that calls tools.
                active_call_ids.clear();
            }
            Message::Tool { tool_call_id, .. } => {
                if !active_call_ids.contains(tool_call_id) {
                    drop_indices.insert(idx);
                }
            }
            _ => {}
        }
    }
    let dropped = drop_indices.len();
    // Single-pass O(n) removal: `retain` shifts each kept element at most
    // once. The previous `Vec::remove(idx)` in reverse was O(n) per drop,
    // i.e. O(n²) when many orphans land in the same history (e.g. a
    // truncated long tool-using turn replayed at session resume).
    let mut idx = 0usize;
    messages.retain(|_| {
        let keep = !drop_indices.contains(&idx);
        idx += 1;
        keep
    });
    dropped
}

impl Agent {
    /// YYC-208: run_prompt with caller-supplied cancellation. Used
    /// by `SpawnSubagentTool` to plumb the parent's CancellationToken
    /// into the child so cancelling the parent turn aborts the
    /// child's loop. The token replaces the agent's `turn_cancel`
    /// for the duration of this run.
    pub async fn run_prompt_with_cancel(
        &mut self,
        input: &str,
        cancel: CancellationToken,
    ) -> Result<String> {
        self.turn_cancel = cancel;
        self.run_prompt_inner(input).await
    }

    pub async fn run_prompt(&mut self, input: &str) -> Result<String> {
        // Fresh token for this turn — calling cancel_current_turn between
        // turns shouldn't affect the next one.
        self.turn_cancel = CancellationToken::new();
        self.run_prompt_inner(input).await
    }

    async fn run_prompt_inner(&mut self, input: &str) -> Result<String> {
        let cancel = self.turn_cancel.clone();

        let system = self
            .prompt_builder
            .build_system_prompt_with_context(&self.tools, Some(&self.tool_context));
        let tool_defs = self
            .tools
            .definitions_with_context(Some(&self.tool_context));
        let mut messages = vec![Message::System { content: system }];

        // Load history for *this* agent's session — set by `resume_session` or
        // `continue_last_session`; defaults to a fresh UUID so a new agent has
        // empty history.
        if let Some(history) = self.memory.load_history(&self.session_id)? {
            for msg in history {
                messages.push(msg);
            }
        }
        // YYC-138: orphan Tool rows from a truncated previous turn would
        // make the provider reject this request with "Tool message must
        // follow tool_calls". Drop them on read; surface a warning so
        // the underlying truncation can still be diagnosed.
        let dropped = sanitize_orphan_tool_messages(&mut messages);
        if dropped > 0 {
            tracing::warn!("agent: dropped {dropped} orphan Tool message(s) from loaded history");
            // Persist the cleaned snapshot so subsequent loads start clean.
            self.replace_history(&messages)?;
        }
        self.last_saved_count = messages.len();

        messages.push(Message::User {
            content: input.to_string(),
        });

        // YYC-106: persist the user message immediately (mirrors the streaming
        // path) so a non-terminal exit doesn't leave the turn unrecorded.
        self.save_messages(&messages)?;

        let max_iter = if self.max_iterations > 0 {
            self.max_iterations as usize
        } else {
            usize::MAX
        };
        for iteration in 0..max_iter {
            tracing::debug!("Agent iteration {iteration}");

            if self.context.should_compact(&messages) {
                self.compact_buffered_messages_if_possible(&mut messages, cancel.clone())
                    .await;
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
                // YYC-211: accumulate across the whole run so
                // spawn_subagent (and future budget enforcement)
                // can see the real token cost, not just the last
                // prompt's size.
                self.tokens_consumed = self
                    .tokens_consumed
                    .saturating_add(usage.total_tokens as u64);
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
                // YYC-106: persist the assistant turn + tool results before
                // the next iteration. Without this, an early loop exit
                // (cancel, max_iter, model returning empty without matching
                // the terminal branch) loses everything since the start of
                // the turn — next prompt resumes with stale history and
                // the agent appears to "forget".
                self.save_messages(&messages)?;
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

                messages.push(Message::Assistant {
                    content: Some(text.clone()),
                    tool_calls: None,
                    reasoning_content: reasoning,
                });
                self.save_messages(&messages)?;
                self.turns = self.turns.saturating_add(1);
                if iteration >= 5 {
                    let _ = self.auto_create_skill_from_turn(input, &text).await;
                }
                return Ok(text);
            }
        }

        Ok("Agent reached maximum iteration limit.".to_string())
    }

    /// Run a prompt with streaming — sends text tokens through `ui_tx` as they
    /// arrive. Honors all hook events. Internal cancel token is fresh per
    /// turn; callers that need to fire the cancel without holding the
    /// `Agent` mutex should use `run_prompt_stream_with_cancel` instead.
    pub async fn run_prompt_stream(
        &mut self,
        input: &str,
        ui_tx: mpsc::Sender<StreamEvent>,
    ) -> Result<String> {
        let cancel = CancellationToken::new();
        self.run_prompt_stream_with_cancel(input, ui_tx, cancel)
            .await
    }

    /// Streaming variant that accepts an external cancel token. The TUI uses
    /// this so the Ctrl+C handler can fire the token directly without
    /// blocking on the agent mutex held by the in-flight prompt task.
    pub async fn run_prompt_stream_with_cancel(
        &mut self,
        input: &str,
        ui_tx: mpsc::Sender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Result<String> {
        let StreamTurn {
            mut messages,
            tool_defs,
        } = self.prepare_stream_turn(input, cancel.clone()).await?;

        let mut full_response = String::new();
        // YYC-104: track tool calls + last usage across iterations so the
        // empty-terminal-turn message can describe what happened.
        let mut tool_calls_total: usize = 0;
        let mut last_usage: Option<Usage> = None;

        let max_iter = if self.max_iterations > 0 {
            self.max_iterations as usize
        } else {
            usize::MAX
        };
        for iteration in 0..max_iter {
            if cancel.is_cancelled() {
                let _ = ui_tx
                    .send(StreamEvent::Done(crate::provider::ChatResponse {
                        content: Some("Cancelled".into()),
                        tool_calls: None,
                        usage: None,
                        finish_reason: Some("cancelled".into()),
                        reasoning_content: None,
                    }))
                    .await;
                return Ok("Cancelled".to_string());
            }

            self.compact_stream_messages_if_needed(&mut messages, input, &ui_tx, iteration)
                .await;

            tracing::info!(
                "agent iteration {iteration} starting (messages={})",
                messages.len()
            );

            // ── BeforePrompt (transient — see run_prompt for rationale).
            let outgoing = self
                .hooks
                .apply_before_prompt(&messages, cancel.clone())
                .await;

            let response = self
                .collect_stream_response(&outgoing, &tool_defs, &ui_tx, cancel.clone(), iteration)
                .await?;

            // YYC-105: feed usage into the context manager so future
            // iterations' should_compact() see realistic numbers.
            if let Some(usage) = &response.usage {
                self.context
                    .record_usage(usage.prompt_tokens, usage.completion_tokens);
                // YYC-211: cumulative tally across the run.
                self.tokens_consumed = self
                    .tokens_consumed
                    .saturating_add(usage.total_tokens as u64);
                last_usage = Some(usage.clone());
            }

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
                tool_calls_total = tool_calls_total.saturating_add(tool_calls.len());
                messages.push(Message::Assistant {
                    content: response.content.clone(),
                    tool_calls: Some(tool_calls.clone()),
                    reasoning_content: response.reasoning_content.clone(),
                });

                let results = self
                    .execute_stream_tool_calls(tool_calls, &ui_tx, cancel.clone())
                    .await;
                for (id, result) in results {
                    messages.push(Message::Tool {
                        tool_call_id: id,
                        content: flatten_for_message(result),
                    });
                }
                // YYC-106: persist the assistant turn + tool results before
                // the next iteration. Without this, an early loop exit
                // (cancel, max_iter, model returning empty without matching
                // the terminal branch) loses everything since the start of
                // the turn — next prompt resumes with stale history and
                // the agent appears to "forget".
                self.save_messages(&messages)?;
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
                    let _ = self
                        .auto_create_skill_from_turn(input, &full_response)
                        .await;
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
                        prompt_tokens = last_usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                        max_context = max_ctx,
                        "agent: model returned empty content with no tool calls",
                    );
                    full_response = hint.clone();
                    let _ = ui_tx.send(StreamEvent::Text(hint)).await;
                }

                let _ = ui_tx
                    .send(StreamEvent::Done(crate::provider::ChatResponse {
                        content: Some(full_response.clone()),
                        tool_calls: None,
                        usage: response.usage,
                        finish_reason: response.finish_reason,
                        reasoning_content: reasoning,
                    }))
                    .await;
                return Ok(full_response);
            }
        }

        // Send a Done event so the TUI exits thinking mode, even
        // though there's no text-only final turn. The loop maxed out
        // at 10 iterations of tool calls — without this, the UI hangs
        // in thinking=true forever (YYC-76).
        let _ = ui_tx
            .send(StreamEvent::Text(
                "Agent reached maximum iteration limit.".into(),
            ))
            .await;
        let _ = ui_tx
            .send(StreamEvent::Done(crate::provider::ChatResponse {
                content: Some("Agent reached maximum iteration limit.".into()),
                tool_calls: None,
                usage: None,
                finish_reason: Some("max_iterations".into()),
                reasoning_content: None,
            }))
            .await;
        Ok("Agent reached maximum iteration limit.".to_string())
    }

    pub(in crate::agent) async fn prepare_stream_turn(
        &mut self,
        input: &str,
        cancel: CancellationToken,
    ) -> Result<StreamTurn> {
        // Mirror the external token onto `self.turn_cancel` so internal
        // tool dispatch / hook plumbing that still references `self.turn_cancel`
        // sees the cancellation. The external token is the source of truth.
        self.turn_cancel = cancel;

        let system = self
            .prompt_builder
            .build_system_prompt_with_context(&self.tools, Some(&self.tool_context));
        let tool_defs = self
            .tools
            .definitions_with_context(Some(&self.tool_context));
        let mut messages = vec![Message::System { content: system }];

        if let Some(history) = self.memory.load_history(&self.session_id)? {
            for msg in history {
                messages.push(msg);
            }
        }
        // YYC-138: heal any orphan Tool rows persisted from a previously
        // truncated turn before the provider sees them.
        let dropped = sanitize_orphan_tool_messages(&mut messages);
        if dropped > 0 {
            tracing::warn!("agent: dropped {dropped} orphan Tool message(s) from loaded history");
            self.replace_history(&messages)?;
        }
        self.last_saved_count = messages.len();

        messages.push(Message::User {
            content: input.to_string(),
        });

        // YYC-106: persist the user message immediately so a later cancel,
        // tool-loop early exit, or process kill doesn't strand the prompt.
        // save_messages tracks last_saved_count and appends only the new tail,
        // so this is cheap.
        self.save_messages(&messages)?;

        Ok(StreamTurn {
            messages,
            tool_defs,
        })
    }

    pub(in crate::agent) async fn compact_stream_messages_if_needed(
        &mut self,
        messages: &mut Vec<Message>,
        _input: &str,
        ui_tx: &mpsc::Sender<StreamEvent>,
        iteration: usize,
    ) {
        // YYC-105 + YYC-128: when the accumulated history would push the next
        // request past the configured trigger ratio, ask the provider to
        // summarize the older turns and splice the summary in place of them.
        // Without this, small-context models (llama-cpp Q2_0 quants, etc.)
        // silently truncate the request and behave as if the session has no
        // history.
        if !self.context.should_compact(messages) {
            return;
        }

        let pre_count = messages.len();
        let cancel = self.turn_cancel.clone();
        let compacted = self
            .compact_buffered_messages_if_possible(messages, cancel)
            .await;
        if compacted {
            let kept_count = pre_count.saturating_sub(messages.len()).max(1);
            let _ = ui_tx
                .send(StreamEvent::Text(format!(
                    "_(compacted {kept_count} earlier turns into a summary to fit context)_\n"
                )))
                .await;
            tracing::info!("agent iteration {iteration}: compacted {kept_count} prior messages");
        } else {
            tracing::warn!(
                "agent iteration {iteration}: compaction skipped (no safe split or summarizer call failed)"
            );
        }
    }

    /// Summarize an older slice of `messages` via a provider call and splice
    /// the summary in place of the slice. Preserves the leading System
    /// prompt and the trailing window past the safe split index.
    ///
    /// Returns `true` when the buffer was actually rewritten — caller can use
    /// that to surface a UX note. Returns `false` when:
    /// - no User-message boundary exists in the trailing window, or
    /// - the summarizer call returns an empty body / errors.
    ///
    /// Failures are logged at warn level and the buffer is left untouched
    /// so the agent can continue with the full history (the next provider
    /// call may simply fail with a context-overflow error, which is still
    /// strictly better than silently losing the entire session — YYC-128).
    pub(in crate::agent) async fn compact_buffered_messages_if_possible(
        &mut self,
        messages: &mut Vec<Message>,
        cancel: CancellationToken,
    ) -> bool {
        use crate::context::ContextManager;

        let split = match self.context.safe_split_index(messages) {
            Some(i) if i > 1 => i, // need at least one message to summarize
            _ => return false,
        };

        let request = ContextManager::summarization_request(&messages[1..split]);
        let response = match self.provider.chat(&request, &[], cancel).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("compaction summarizer call failed: {e}");
                return false;
            }
        };
        let summary = response.content.unwrap_or_default();
        if summary.trim().is_empty() {
            tracing::warn!("compaction summarizer returned empty body");
            return false;
        }

        let mut new_messages = Vec::with_capacity(messages.len() - split + 2);
        new_messages.push(messages[0].clone());
        new_messages.push(Message::System {
            content: format!("Summary of earlier conversation:\n{summary}"),
        });
        new_messages.extend(messages[split..].iter().cloned());
        *messages = new_messages;

        self.context.install_summary(summary);
        // YYC-138: persist the rewritten snapshot atomically so the next
        // save_messages append doesn't orphan Tool rows from the dropped
        // pre-summary slice.
        if let Err(e) = self.replace_history(messages) {
            tracing::warn!("failed to replace persisted history after compaction: {e}");
        }
        true
    }

    pub(in crate::agent) async fn collect_stream_response(
        &self,
        outgoing: &[Message],
        tool_defs: &[ToolDefinition],
        ui_tx: &mpsc::Sender<StreamEvent>,
        cancel: CancellationToken,
        iteration: usize,
    ) -> Result<ChatResponse> {
        // YYC-147: capacity comes from the active provider config so
        // operators can tune it for slow renderers / fast providers.
        let stream_cap = self.provider_config.effective_stream_channel_capacity();
        let (inner_tx, mut inner_rx) = mpsc::channel::<StreamEvent>(stream_cap);
        let (priv_tx, mut priv_rx) = mpsc::channel::<StreamEvent>(stream_cap);

        let ui_tx_clone = ui_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = inner_rx.recv().await {
                match &ev {
                    StreamEvent::Text(_) => {
                        let _ = ui_tx_clone.send(ev).await;
                    }
                    StreamEvent::Done(_) | StreamEvent::Error(_) => {
                        let _ = priv_tx.send(ev).await;
                        break;
                    }
                    _ => {
                        let _ = ui_tx_clone.send(ev).await;
                    }
                }
            }
        });

        if let Err(e) = self
            .provider
            .chat_stream(outgoing, tool_defs, inner_tx, cancel)
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
            let _ = ui_tx.send(StreamEvent::Error(user_message.clone())).await;
            let _ = ui_tx
                .send(StreamEvent::Done(ChatResponse {
                    content: Some(format!("⚠ {user_message}")),
                    tool_calls: None,
                    usage: None,
                    finish_reason: Some("error".into()),
                    reasoning_content: None,
                }))
                .await;
            return Err(e);
        }

        let mut final_response: Option<ChatResponse> = None;
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

        match final_response {
            Some(r) => Ok(r),
            None => {
                let msg = "Stream ended without Done event";
                tracing::error!("agent iteration {iteration}: {msg}");
                let _ = ui_tx.send(StreamEvent::Error(msg.into())).await;
                Err(anyhow::anyhow!(msg))
            }
        }
    }

    pub(in crate::agent) async fn execute_stream_tool_calls(
        &self,
        tool_calls: &[ToolCall],
        ui_tx: &mpsc::Sender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Vec<(String, ToolResult)> {
        // YYC-34: dispatch all calls concurrently. ToolCallStart events fire
        // synchronously up front so the TUI shows every in-flight tool
        // immediately; ToolCallEnd fires from inside each future as it
        // completes (so they may arrive out of order — that's the point).
        // `join_all` preserves order for the message vector so tool_call_id
        // alignment stays deterministic.
        for tc in tool_calls {
            // YYC-74: derive a one-line args summary so the TUI's tool-card
            // has structured context to show.
            let args_summary = summarize_tool_args(&tc.function.name, &tc.function.arguments);
            let _ = ui_tx
                .send(StreamEvent::ToolCallStart {
                    id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    args_summary,
                })
                .await;
        }

        let dispatches = tool_calls.iter().map(|tc| {
            let id = tc.id.clone();
            let name = tc.function.name.clone();
            let args = tc.function.arguments.clone();
            let cancel = cancel.clone();
            let ui_tx = ui_tx.clone();
            async move {
                tracing::info!("Executing tool: {name} (call {id})");
                let started = std::time::Instant::now();
                let result = self.dispatch_tool(&name, &args, cancel).await;
                let elapsed_ms = started.elapsed().as_millis() as u64;
                let ok = !result.is_error;
                // YYC-74: truncated output preview + meta line.
                // YYC-78: elided line count for the auto-collapse
                // "N more lines" indicator.
                // Tools like write_file/edit_file populate
                // `display_preview` with a real diff; prefer it
                // over the LLM-facing terse `output`.
                let preview_source = result.display_preview.as_deref().unwrap_or(&result.output);
                let output_preview = preview_output(preview_source);
                let result_meta = summarize_tool_result(&name, &result.output);
                let elided = elided_lines(preview_source, output_preview.as_deref());
                let _ = ui_tx
                    .send(StreamEvent::ToolCallEnd {
                        id: id.clone(),
                        name: name.clone(),
                        ok,
                        output_preview,
                        result_meta,
                        elided_lines: elided,
                        elapsed_ms,
                    })
                    .await;
                (id, result)
            }
        });
        futures_util::future::join_all(dispatches).await
    }
}
