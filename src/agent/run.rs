//! Streaming + buffered prompt execution extracted from `agent/mod.rs`
//! (YYC-109 redo). Owns `run_prompt`, `run_prompt_stream(_with_cancel)`,
//! the streaming pipeline helpers (prepare/compact/collect/execute), the
//! orphan-tool sanitizer, and the empty-terminal hint.

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::provider::{ChatResponse, Message, StreamEvent, ToolCall, ToolDefinition, Usage};
use crate::run_record::{PayloadFingerprint, RunEvent, RunOrigin, RunRecord, RunStatus};
use crate::tools::ToolResult;

use super::dispatch::{elided_lines, preview_output, summarize_tool_args, summarize_tool_result};
use super::turn::{TurnEvent, TurnMode, TurnOutcome, TurnRunner, TurnRunnerMut, TurnStatus};
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

    /// GH issue #557: invoke registered `on_input` hooks against
    /// `raw` and surface the decision. Daemon entry points
    /// (`prompt.run`, `prompt.stream`, gateway lane drains) call
    /// this before slash dispatch / `run_prompt_*` so extensions can
    /// block or rewrite raw user input. Audit log records every
    /// non-Continue outcome via the registry's installed audit log.
    pub async fn apply_on_input(&self, raw: &str) -> crate::hooks::InputDecision {
        self.hooks
            .apply_on_input(raw, self.turn_cancel.clone())
            .await
    }

    /// Slice 7: like [`Self::run_prompt_with_cancel`] but stamps the
    /// run record's `RunOrigin` so child runs land as
    /// `RunOrigin::Subagent { parent_run_id }` and `vulcan run show`
    /// can render the parent → child timeline without joining
    /// against orchestration metadata.
    pub async fn run_prompt_with_cancel_origin(
        &mut self,
        input: &str,
        cancel: CancellationToken,
        origin: RunOrigin,
    ) -> Result<String> {
        self.turn_cancel = cancel;
        self.begin_run_record_with_origin(input, origin);
        let result = self.run_prompt_body(input).await;
        match &result {
            Ok(text) if text == "Cancelled" => self.end_run_record(RunStatus::Cancelled, None),
            Ok(_) => self.end_run_record(RunStatus::Completed, None),
            Err(e) => self.end_run_record(RunStatus::Failed, Some(e.to_string())),
        }
        result
    }

    pub async fn run_prompt(&mut self, input: &str) -> Result<String> {
        // Fresh token for this turn — calling cancel_current_turn between
        // turns shouldn't affect the next one.
        self.turn_cancel = CancellationToken::new();
        self.run_prompt_inner(input).await
    }

    /// YYC-179: open a run record for the current turn, marking it
    /// `Running`. Writes a `PromptReceived` event with a SHA-256
    /// fingerprint of the input — raw text isn't persisted by
    /// default. Returns the new `RunId` so the caller can attach
    /// further events.
    fn begin_run_record(&mut self, input: &str) {
        self.begin_run_record_with_origin(input, RunOrigin::Cli);
    }

    fn begin_run_record_with_origin(&mut self, input: &str, origin: RunOrigin) {
        let mut record = RunRecord::new(origin);
        record.session_id = Some(self.session_id.clone());
        record.model = Some(self.provider_config.model.clone());
        let id = record.id;
        if let Err(e) = self.run_store.create(&record) {
            tracing::warn!("run_record create failed: {e}");
            *self.current_run_id.lock() = None;
            return;
        }
        *self.current_run_id.lock() = Some(id);
        let _ = self.run_store.append_event(
            id,
            RunEvent::StatusChanged {
                status: RunStatus::Running,
            },
        );
        // YYC-182: stamp the trust profile up front so timeline
        // viewers can see the policy posture for this turn.
        let trust = &self.trust_profile;
        let _ = self.run_store.append_event(
            id,
            RunEvent::TrustResolved {
                level: trust.level.as_str().to_string(),
                capability_profile: trust.capability_profile.clone(),
                reason: trust.reason.clone(),
                allow_indexing: trust.allow_indexing,
                allow_persistence: trust.allow_persistence,
            },
        );
        let _ = self.run_store.append_event(
            id,
            RunEvent::PromptReceived {
                fingerprint: PayloadFingerprint::of(input.as_bytes()),
                char_count: input.chars().count(),
                raw: None,
            },
        );
    }

    /// YYC-179: write the terminal status for the current run and
    /// clear `current_run_id`. Safe to call when no run is active.
    fn end_run_record(&mut self, status: RunStatus, error: Option<String>) {
        if let Some(id) = self.current_run_id.lock().take() {
            let _ = self.run_store.finalize(id, status, error);
        }
    }

    /// YYC-179: append a typed event to the active run record, if
    /// any. Drops silently when no run is in flight (e.g. a tool
    /// running outside a turn) so callers don't have to gate their
    /// emit sites.
    pub(in crate::agent) fn record_run_event(&self, event: RunEvent) {
        if let Some(id) = *self.current_run_id.lock() {
            if let Err(e) = self.run_store.append_event(id, event) {
                tracing::warn!("run_record append failed: {e}");
            }
        }
    }

    async fn run_prompt_inner(&mut self, input: &str) -> Result<String> {
        self.begin_run_record(input);
        let result = self.run_prompt_body(input).await;
        match &result {
            Ok(text) if text == "Cancelled" => {
                self.end_run_record(RunStatus::Cancelled, None);
            }
            Ok(_) => {
                self.end_run_record(RunStatus::Completed, None);
            }
            Err(e) => {
                self.end_run_record(RunStatus::Failed, Some(e.to_string()));
            }
        }
        result
    }

    async fn run_prompt_body(&mut self, input: &str) -> Result<String> {
        let cancel = self.turn_cancel.clone();
        let cap = self.provider_config.effective_stream_channel_capacity();
        let (events_tx, mut events_rx) = mpsc::channel::<TurnEvent>(cap);
        // Buffered callers don't render TurnEvents — drain so the runner's
        // sends never block on a full channel.
        let drainer = tokio::spawn(async move { while events_rx.recv().await.is_some() {} });

        let outcome = TurnRunnerMut::new(self)
            .run(input, cancel, &events_tx, TurnMode::Buffered)
            .await;
        drop(events_tx);
        let _ = drainer.await;
        outcome.map(|o| o.final_text)
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

    /// YYC-179: streaming entry point for gateway lanes. Identical
    /// to `run_prompt_stream_with_cancel` but tags the run record's
    /// origin as `RunOrigin::Gateway { lane }` so the timeline
    /// surfaces lane attribution.
    pub async fn run_prompt_stream_for_gateway(
        &mut self,
        input: &str,
        ui_tx: mpsc::Sender<StreamEvent>,
        cancel: CancellationToken,
        lane: String,
    ) -> Result<String> {
        self.begin_run_record_with_origin(input, RunOrigin::Gateway { lane });
        let result = self.run_prompt_stream_body(input, ui_tx, cancel).await;
        match &result {
            Ok(text) if text == "Cancelled" => {
                self.end_run_record(RunStatus::Cancelled, None);
            }
            Ok(_) => {
                self.end_run_record(RunStatus::Completed, None);
            }
            Err(e) => {
                self.end_run_record(RunStatus::Failed, Some(e.to_string()));
            }
        }
        result
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
        // YYC-179: open a run record for the streaming turn too. The
        // TUI is the primary streaming consumer, so the resulting
        // origin is `Tui` rather than `Cli`.
        self.begin_run_record_with_origin(input, RunOrigin::Tui);
        let result = self.run_prompt_stream_body(input, ui_tx, cancel).await;
        match &result {
            Ok(text) if text == "Cancelled" => {
                self.end_run_record(RunStatus::Cancelled, None);
            }
            Ok(_) => {
                self.end_run_record(RunStatus::Completed, None);
            }
            Err(e) => {
                self.end_run_record(RunStatus::Failed, Some(e.to_string()));
            }
        }
        result
    }

    async fn run_prompt_stream_body(
        &mut self,
        input: &str,
        ui_tx: mpsc::Sender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Result<String> {
        let cap = self.provider_config.effective_stream_channel_capacity();
        let (events_tx, mut events_rx) = mpsc::channel::<TurnEvent>(cap);
        let ui_tx_forward = ui_tx.clone();
        let forwarder = tokio::spawn(async move {
            while let Some(event) = events_rx.recv().await {
                if matches!(event, TurnEvent::ProviderDone { .. }) {
                    continue;
                }
                let _ = ui_tx_forward.send(StreamEvent::from(event)).await;
            }
        });

        let result = TurnRunnerMut::new(self)
            .run(input, cancel, &events_tx, TurnMode::Streaming)
            .await;
        drop(events_tx);
        let _ = forwarder.await;

        match result {
            Ok(outcome) => {
                // Max-iter UX preserves the legacy text+done pair so the TUI
                // exits thinking mode with a readable banner.
                if matches!(outcome.status, TurnStatus::MaxIterations) {
                    let _ = ui_tx
                        .send(StreamEvent::Text(outcome.final_text.clone()))
                        .await;
                }
                if let Some(response) = outcome.final_response {
                    let _ = ui_tx.send(StreamEvent::Done(response)).await;
                }
                Ok(outcome.final_text)
            }
            Err(e) => Err(e),
        }
    }

    pub(in crate::agent) async fn prepare_stream_turn(
        &mut self,
        input: &str,
        cancel: CancellationToken,
    ) -> Result<StreamTurn> {
        self.prepare_turn(input, cancel).await
    }

    pub(in crate::agent) async fn prepare_turn(
        &mut self,
        input: &str,
        cancel: CancellationToken,
    ) -> Result<StreamTurn> {
        TurnRunnerMut::new(self).prepare(input, cancel).await
    }

    async fn prepare_turn_impl(
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

        // Slice 2: load + sanitize + heal once on first prepare; later
        // turns reuse the in-memory snapshot. Storage stays as the
        // durability + recovery source, not the hot-path source of
        // truth.
        if !self.history_loaded {
            if let Some(history) = self.memory.load_history(&self.session_id)? {
                for msg in history {
                    messages.push(msg);
                }
            }
            // YYC-138: heal any orphan Tool rows persisted from a
            // previously truncated turn before the provider sees them.
            let dropped = sanitize_orphan_tool_messages(&mut messages);
            if dropped > 0 {
                tracing::warn!(
                    "agent: dropped {dropped} orphan Tool message(s) from loaded history"
                );
                self.replace_history(&messages)?;
            } else {
                self.history_cache = messages.iter().skip(1).cloned().collect();
                self.history_loaded = true;
            }
        } else {
            messages.extend(self.history_cache.iter().cloned());
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
        let stream_cap = self.provider_config.effective_stream_channel_capacity();
        let (turn_tx, mut turn_rx) = mpsc::channel::<TurnEvent>(stream_cap);
        let ui_tx_clone = ui_tx.clone();

        let forwarder = tokio::spawn(async move {
            while let Some(event) = turn_rx.recv().await {
                let _ = ui_tx_clone.send(StreamEvent::from(event)).await;
            }
        });

        self.compact_turn_messages_if_needed(messages, &turn_tx, iteration)
            .await;
        drop(turn_tx);
        let _ = forwarder.await;
    }

    pub(in crate::agent) async fn compact_turn_messages_if_needed(
        &mut self,
        messages: &mut Vec<Message>,
        turn_tx: &mpsc::Sender<TurnEvent>,
        iteration: usize,
    ) {
        TurnRunnerMut::new(self)
            .compact_messages_if_needed(messages, turn_tx, iteration)
            .await
    }

    async fn compact_turn_messages_if_needed_impl(
        &mut self,
        messages: &mut Vec<Message>,
        turn_tx: &mpsc::Sender<TurnEvent>,
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
        if !self
            .hooks
            .on_session_before_compact(self.turn_cancel.clone())
            .await
        {
            tracing::info!("agent iteration {iteration}: compaction blocked by hook");
            return;
        }

        let pre_count = messages.len();
        let cancel = self.turn_cancel.clone();
        let compacted = self
            .compact_buffered_messages_if_possible(messages, cancel)
            .await;
        if compacted {
            let earlier_messages = pre_count.saturating_sub(messages.len()).max(1);
            let _ = turn_tx
                .send(TurnEvent::Compacted { earlier_messages })
                .await;
            tracing::info!(
                "agent iteration {iteration}: compacted {earlier_messages} prior messages"
            );
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
        let response = match self.provider.chat(&request, &[], cancel.clone()).await {
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

        self.hooks
            .on_session_compact(&summary, cancel.clone())
            .await;
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
        let stream_cap = self.provider_config.effective_stream_channel_capacity();
        let (turn_tx, mut turn_rx) = mpsc::channel::<TurnEvent>(stream_cap);
        let ui_tx_clone = ui_tx.clone();

        tokio::spawn(async move {
            while let Some(event) = turn_rx.recv().await {
                match event {
                    TurnEvent::ProviderDone { .. } | TurnEvent::Error { .. } => break,
                    other => {
                        let _ = ui_tx_clone.send(StreamEvent::from(other)).await;
                    }
                }
            }
        });

        let result = self
            .collect_turn_response(outgoing, tool_defs, &turn_tx, cancel, iteration)
            .await;
        if let Err(e) = &result {
            let user_message = format!("{e}");
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
        }
        result
    }

    pub(in crate::agent) async fn collect_turn_response(
        &self,
        outgoing: &[Message],
        tool_defs: &[ToolDefinition],
        turn_tx: &mpsc::Sender<TurnEvent>,
        cancel: CancellationToken,
        iteration: usize,
    ) -> Result<ChatResponse> {
        TurnRunner::new(self)
            .collect_response(outgoing, tool_defs, turn_tx, cancel, iteration)
            .await
    }

    async fn collect_turn_response_impl(
        &self,
        outgoing: &[Message],
        tool_defs: &[ToolDefinition],
        turn_tx: &mpsc::Sender<TurnEvent>,
        cancel: CancellationToken,
        iteration: usize,
    ) -> Result<ChatResponse> {
        // YYC-147: capacity comes from the active provider config so
        // operators can tune it for slow renderers / fast providers.
        let stream_cap = self.provider_config.effective_stream_channel_capacity();
        let (inner_tx, mut inner_rx) = mpsc::channel::<StreamEvent>(stream_cap);
        let (priv_tx, mut priv_rx) = mpsc::channel::<TurnEvent>(stream_cap);

        let turn_tx_clone = turn_tx.clone();
        let hooks = self.hooks.clone();
        let hook_cancel = cancel.clone();
        tokio::spawn(async move {
            let mut message_started = false;
            while let Some(ev) = inner_rx.recv().await {
                match &ev {
                    StreamEvent::Text(_) | StreamEvent::Reasoning(_) => {
                        if !message_started {
                            hooks.on_message_start(&ev, hook_cancel.clone()).await;
                            message_started = true;
                        }
                        hooks.on_message_update(&ev, hook_cancel.clone()).await;
                    }
                    StreamEvent::Done(_) => {
                        if message_started {
                            hooks.on_message_end(&ev, hook_cancel.clone()).await;
                        }
                    }
                    _ => {}
                }
                let event = TurnEvent::from(ev);
                match &event {
                    TurnEvent::ProviderDone { .. } | TurnEvent::Error { .. } => {
                        let _ = turn_tx_clone.send(event.clone()).await;
                        let _ = priv_tx.send(event).await;
                        break;
                    }
                    _ => {
                        let _ = turn_tx_clone.send(event).await;
                    }
                }
            }
        });

        // YYC-179: record the provider request before dispatch so a
        // transport error still leaves a breadcrumb on the timeline.
        self.record_run_event(RunEvent::ProviderRequest {
            model: self.provider_config.model.clone(),
            streaming: true,
            message_count: outgoing.len(),
        });
        self.hooks
            .on_before_provider_request(outgoing, cancel.clone())
            .await;

        if let Err(e) = self
            .provider
            .chat_stream(outgoing, tool_defs, inner_tx, cancel.clone())
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
            self.record_run_event(RunEvent::ProviderError {
                message: user_message.clone(),
                retryable: false,
            });
            let _ = turn_tx
                .send(TurnEvent::Error {
                    message: user_message.clone(),
                })
                .await;
            let _ = turn_tx
                .send(TurnEvent::ProviderDone {
                    response: ChatResponse {
                        content: Some(format!("⚠ {user_message}")),
                        tool_calls: None,
                        usage: None,
                        finish_reason: Some("error".into()),
                        reasoning_content: None,
                    },
                })
                .await;
            return Err(e);
        }

        let mut final_response: Option<ChatResponse> = None;
        while let Some(event) = priv_rx.recv().await {
            match event {
                TurnEvent::ProviderDone { response } => {
                    final_response = Some(response);
                    break;
                }
                TurnEvent::Error { message } => {
                    return Err(anyhow::anyhow!("{message}"));
                }
                _ => {}
            }
        }

        match final_response {
            Some(r) => {
                self.hooks
                    .on_after_provider_response(&r, cancel.clone())
                    .await;
                // YYC-179: emit ProviderResponse for the streaming
                // path so dashboards can group buffered/streaming
                // turns under the same event family.
                if let Some(usage) = &r.usage {
                    self.record_run_event(RunEvent::ProviderResponse {
                        prompt_tokens: usage.prompt_tokens as u32,
                        completion_tokens: usage.completion_tokens as u32,
                        total_tokens: usage.total_tokens as u32,
                        finish_reason: r.finish_reason.clone(),
                    });
                } else {
                    self.record_run_event(RunEvent::ProviderResponse {
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                        finish_reason: r.finish_reason.clone(),
                    });
                }
                Ok(r)
            }
            None => {
                let msg = "Stream ended without Done event";
                tracing::error!("agent iteration {iteration}: {msg}");
                self.record_run_event(RunEvent::ProviderError {
                    message: msg.to_string(),
                    retryable: false,
                });
                let _ = turn_tx
                    .send(TurnEvent::Error {
                        message: msg.into(),
                    })
                    .await;
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
        let stream_cap = self.provider_config.effective_stream_channel_capacity();
        let (turn_tx, mut turn_rx) = mpsc::channel::<TurnEvent>(stream_cap);
        let ui_tx_clone = ui_tx.clone();

        let forwarder = tokio::spawn(async move {
            while let Some(event) = turn_rx.recv().await {
                let _ = ui_tx_clone.send(StreamEvent::from(event)).await;
            }
        });

        let results = self
            .execute_turn_tool_calls(tool_calls, &turn_tx, cancel)
            .await;
        drop(turn_tx);
        let _ = forwarder.await;
        results
    }

    pub(in crate::agent) async fn execute_turn_tool_calls(
        &self,
        tool_calls: &[ToolCall],
        turn_tx: &mpsc::Sender<TurnEvent>,
        cancel: CancellationToken,
    ) -> Vec<(String, ToolResult)> {
        TurnRunner::new(self)
            .execute_tool_calls(tool_calls, turn_tx, cancel)
            .await
    }

    async fn execute_turn_tool_calls_impl(
        &self,
        tool_calls: &[ToolCall],
        turn_tx: &mpsc::Sender<TurnEvent>,
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
            let _ = turn_tx
                .send(TurnEvent::ToolCallStart {
                    id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    args_summary,
                })
                .await;
        }

        let dispatches = tool_calls.iter().map(|tc| {
            let call = tc.clone();
            let id = tc.id.clone();
            let name = tc.function.name.clone();
            let args = tc.function.arguments.clone();
            let cancel = cancel.clone();
            let turn_tx = turn_tx.clone();
            async move {
                tracing::info!("Executing tool: {name} (call {id})");
                let started = std::time::Instant::now();
                self.hooks
                    .on_tool_execution_start(&call, cancel.clone())
                    .await;
                let (progress_tx, mut progress_rx) =
                    mpsc::channel::<crate::tools::ToolProgress>(16);
                let hooks = self.hooks.clone();
                let progress_call = call.clone();
                let progress_cancel = cancel.clone();
                let progress_task = tokio::spawn(async move {
                    while let Some(progress) = progress_rx.recv().await {
                        hooks
                            .on_tool_execution_update(
                                &progress_call,
                                &progress,
                                progress_cancel.clone(),
                            )
                            .await;
                    }
                });
                let result = self
                    .dispatch_tool(&name, &args, cancel.clone(), Some(progress_tx))
                    .await;
                let _ = progress_task.await;
                self.hooks
                    .on_tool_execution_end(&call, cancel.clone())
                    .await;
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
                let _ = turn_tx
                    .send(TurnEvent::ToolCallEnd {
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

impl TurnRunner<'_> {
    pub(in crate::agent) async fn collect_response(
        &self,
        outgoing: &[Message],
        tool_defs: &[ToolDefinition],
        turn_tx: &mpsc::Sender<TurnEvent>,
        cancel: CancellationToken,
        iteration: usize,
    ) -> Result<ChatResponse> {
        self.agent
            .collect_turn_response_impl(outgoing, tool_defs, turn_tx, cancel, iteration)
            .await
    }

    pub(in crate::agent) async fn execute_tool_calls(
        &self,
        tool_calls: &[ToolCall],
        turn_tx: &mpsc::Sender<TurnEvent>,
        cancel: CancellationToken,
    ) -> Vec<(String, ToolResult)> {
        self.agent
            .execute_turn_tool_calls_impl(tool_calls, turn_tx, cancel)
            .await
    }
}

impl TurnRunnerMut<'_> {
    pub(in crate::agent) async fn prepare(
        &mut self,
        input: &str,
        cancel: CancellationToken,
    ) -> Result<StreamTurn> {
        self.agent.prepare_turn_impl(input, cancel).await
    }

    pub(in crate::agent) async fn compact_messages_if_needed(
        &mut self,
        messages: &mut Vec<Message>,
        turn_tx: &mpsc::Sender<TurnEvent>,
        iteration: usize,
    ) {
        self.agent
            .compact_turn_messages_if_needed_impl(messages, turn_tx, iteration)
            .await;
    }

    /// Unified turn execution. Buffered and streaming entry points become
    /// thin adapters that drain (or forward) the [`TurnEvent`] sink and
    /// translate the [`TurnOutcome`] into their respective return shapes.
    /// One iteration loop, one source of truth for hooks/compaction/tool
    /// dispatch/persistence.
    pub(in crate::agent) async fn run(
        &mut self,
        input: &str,
        cancel: CancellationToken,
        events: &mpsc::Sender<TurnEvent>,
        mode: TurnMode,
    ) -> Result<TurnOutcome> {
        let StreamTurn {
            mut messages,
            tool_defs,
        } = self.prepare(input, cancel.clone()).await?;

        let max_iter = if self.agent.max_iterations > 0 {
            self.agent.max_iterations as usize
        } else {
            usize::MAX
        };

        let mut full_response = String::new();
        let mut tool_calls_total: usize = 0;
        let mut last_usage: Option<Usage> = None;

        for iteration in 0..max_iter {
            if cancel.is_cancelled() {
                let response = ChatResponse {
                    content: Some("Cancelled".into()),
                    tool_calls: None,
                    usage: None,
                    finish_reason: Some("cancelled".into()),
                    reasoning_content: None,
                };
                return Ok(TurnOutcome {
                    final_text: "Cancelled".into(),
                    final_response: Some(response),
                    status: TurnStatus::Cancelled,
                });
            }

            self.agent
                .hooks
                .on_turn_start(iteration as u32, cancel.clone())
                .await;

            self.compact_messages_if_needed(&mut messages, events, iteration)
                .await;

            tracing::info!(
                "agent iteration {iteration} starting (messages={})",
                messages.len()
            );

            let outgoing = self
                .agent
                .hooks
                .apply_context(&messages, cancel.clone())
                .await;

            let response = match mode {
                TurnMode::Buffered => {
                    self.agent.record_run_event(RunEvent::ProviderRequest {
                        model: self.agent.provider_config.model.clone(),
                        streaming: false,
                        message_count: outgoing.len(),
                    });
                    self.agent
                        .hooks
                        .on_before_provider_request(&outgoing, cancel.clone())
                        .await;
                    let resp = match self
                        .agent
                        .provider
                        .chat(&outgoing, &tool_defs, cancel.clone())
                        .await
                    {
                        Ok(r) => r,
                        Err(e) => {
                            self.agent.record_run_event(RunEvent::ProviderError {
                                message: e.to_string(),
                                retryable: false,
                            });
                            return Err(e);
                        }
                    };
                    self.agent
                        .hooks
                        .on_after_provider_response(&resp, cancel.clone())
                        .await;
                    if let Some(usage) = &resp.usage {
                        self.agent.record_run_event(RunEvent::ProviderResponse {
                            prompt_tokens: usage.prompt_tokens as u32,
                            completion_tokens: usage.completion_tokens as u32,
                            total_tokens: usage.total_tokens as u32,
                            finish_reason: resp.finish_reason.clone(),
                        });
                    } else {
                        self.agent.record_run_event(RunEvent::ProviderResponse {
                            prompt_tokens: 0,
                            completion_tokens: 0,
                            total_tokens: 0,
                            finish_reason: resp.finish_reason.clone(),
                        });
                    }
                    if let Some(text) = &resp.content {
                        // Buffered path doesn't naturally fan-out chunks;
                        // emit one TurnEvent::Text so adapters that forward
                        // events still see assistant content.
                        if !text.is_empty() {
                            let event = StreamEvent::Text(text.clone());
                            self.agent
                                .hooks
                                .on_message_start(&event, cancel.clone())
                                .await;
                            self.agent
                                .hooks
                                .on_message_update(&event, cancel.clone())
                                .await;
                            self.agent
                                .hooks
                                .on_message_end(&event, cancel.clone())
                                .await;
                            let _ = events.send(TurnEvent::Text { text: text.clone() }).await;
                        }
                    }
                    resp
                }
                TurnMode::Streaming => {
                    self.agent
                        .collect_turn_response_impl(
                            &outgoing,
                            &tool_defs,
                            events,
                            cancel.clone(),
                            iteration,
                        )
                        .await?
                }
            };

            if let Some(usage) = &response.usage {
                self.agent
                    .context
                    .record_usage(usage.prompt_tokens, usage.completion_tokens);
                self.agent.tokens_consumed = self
                    .agent
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
                    .agent
                    .execute_turn_tool_calls_impl(tool_calls, events, cancel.clone())
                    .await;
                for (id, result) in results {
                    messages.push(Message::Tool {
                        tool_call_id: id,
                        content: flatten_for_message(result),
                    });
                }
                self.agent.save_messages(&messages)?;
                self.agent
                    .hooks
                    .on_turn_end(iteration as u32, cancel.clone())
                    .await;
            } else {
                let reasoning = response.reasoning_content.clone();

                if let Some(instruction) = self
                    .agent
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
                    self.agent
                        .hooks
                        .on_turn_end(iteration as u32, cancel.clone())
                        .await;
                    continue;
                }

                messages.push(Message::Assistant {
                    content: Some(full_response.clone()),
                    tool_calls: None,
                    reasoning_content: reasoning.clone(),
                });
                self.agent.save_messages(&messages)?;
                self.agent.turns = self.agent.turns.saturating_add(1);
                if iteration >= 5 {
                    let _ = self
                        .agent
                        .auto_create_skill_from_turn(input, &full_response)
                        .await;
                }

                if matches!(mode, TurnMode::Streaming) && full_response.is_empty() {
                    let reasoning_len = reasoning.as_deref().map(str::len).unwrap_or(0);
                    let max_ctx = self.agent.provider.max_context();
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
                    let _ = events.send(TurnEvent::Text { text: hint }).await;
                }

                let final_response = ChatResponse {
                    content: Some(full_response.clone()),
                    tool_calls: None,
                    usage: response.usage,
                    finish_reason: response.finish_reason,
                    reasoning_content: reasoning,
                };
                self.agent
                    .hooks
                    .on_turn_end(iteration as u32, cancel.clone())
                    .await;
                return Ok(TurnOutcome {
                    final_text: full_response,
                    final_response: Some(final_response),
                    status: TurnStatus::Completed,
                });
            }
        }

        let max_text = "Agent reached maximum iteration limit.".to_string();
        let response = ChatResponse {
            content: Some(max_text.clone()),
            tool_calls: None,
            usage: None,
            finish_reason: Some("max_iterations".into()),
            reasoning_content: None,
        };
        self.agent
            .hooks
            .on_turn_end(max_iter as u32, cancel.clone())
            .await;
        Ok(TurnOutcome {
            final_text: max_text,
            final_response: Some(response),
            status: TurnStatus::MaxIterations,
        })
    }
}
