//! Pi-style extension hooks.
//!
//! Five wire-in points in the agent loop emit events; registered handlers may
//! return outcomes that block, modify, or extend the in-flight operation.
//! Errors and timeouts in handlers are isolated — they never break the agent
//! loop. First non-Continue outcome wins for blocking-style events; injection
//! events accumulate across all handlers.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::pause::{AgentPause, AgentResume, OptionKind, PauseKind, PauseOption, PauseSender};
use crate::provider::{ChatResponse, Message, StreamEvent, ToolCall};
use crate::tools::{ToolProgress, ToolResult};

pub mod audit;
pub mod safety;
pub mod skills;

/// Where injected messages land in the outgoing prompt. Only honored by
/// `before_prompt` injections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InjectPosition {
    /// Insert immediately after the leading run of System messages, before any
    /// User/Assistant/Tool turns. Right slot for "static context" hooks like
    /// available skills.
    AfterSystem,
    /// Append to the end of the outgoing message list. Right slot for
    /// "reminders" that should be the last thing the model sees.
    Append,
}

pub mod approval;
pub mod cortex_capture;
pub mod cortex_recall;
pub mod diagnostics;
pub mod prefer_native;
pub mod recall;
pub mod rtk;

/// What a handler returns. Each event honors a subset; unsupported variants
/// are logged and ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HookOutcome {
    /// Default — handler observed but does not change behavior.
    Continue,
    /// Refuse the in-flight operation. For tool calls this short-circuits the
    /// dispatch and substitutes the reason as the tool result.
    Block { reason: String },
    /// Replace the tool arguments before dispatch (BeforeToolCall only).
    ReplaceArgs(Value),
    /// Replace the tool result before it goes back to the LLM (AfterToolCall).
    ReplaceResult(ToolResult),
    /// Inject messages into the outgoing LLM prompt at the requested position
    /// (BeforePrompt only). Injections are transient — they go on the wire but
    /// are not stored in conversation history.
    InjectMessages {
        messages: Vec<Message>,
        position: InjectPosition,
    },
    /// Transiently replace the outgoing message payload. Intended for
    /// `on_context`; the persistent conversation history is not mutated.
    RewriteMessages(Vec<Message>),
    /// Durably replace Session History during `on_session_before_compact`.
    /// This is validated by the turn runner before it can replace history.
    RewriteHistory(Vec<Message>),
    /// Force the agent to keep working; the instruction is appended as a user
    /// turn and the loop continues (BeforeAgentEnd only).
    ForceContinue { instruction: String },
    /// Refuse raw user input before a turn starts. Intended for `on_input`.
    BlockInput { reason: String },
    /// Replace raw user input before slash dispatch and turn execution.
    ReplaceInput(String),
}

/// Decision returned to the agent loop by `before_tool_call`.
#[derive(Debug, Clone)]
pub enum ToolCallDecision {
    Continue,
    Block(String),
    ReplaceArgs(Value),
}

/// Decision returned to the agent / daemon loop by `on_input`.
/// `Block` short-circuits the prompt entirely; `Replace` swaps the raw
/// user text before slash dispatch and turn execution; `Continue`
/// passes the original input through unchanged.
#[derive(Debug, Clone)]
pub enum InputDecision {
    Continue,
    Block(String),
    Replace(String),
}

/// Decision returned to the turn runner by `on_session_before_compact`.
#[derive(Debug, Clone)]
pub enum CompactionDecision {
    Continue,
    Block {
        extension_id: String,
        reason: String,
    },
    RewriteHistory {
        extension_id: String,
        messages: Vec<Message>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum RewriteRejection {
    MissingSystem,
    OrphanToolCallId {
        tool_call_id: String,
    },
    NotShorter {
        input_len: usize,
        proposed_len: usize,
    },
}

pub fn validate_rewrite_history(
    input: &[Message],
    proposed: &[Message],
) -> Result<(), RewriteRejection> {
    if proposed.len() >= input.len() {
        return Err(RewriteRejection::NotShorter {
            input_len: input.len(),
            proposed_len: proposed.len(),
        });
    }
    if !proposed
        .iter()
        .any(|msg| matches!(msg, Message::System { .. }))
    {
        return Err(RewriteRejection::MissingSystem);
    }

    let mut active_tool_call_ids: HashSet<&str> = HashSet::new();
    for msg in proposed {
        match msg {
            Message::Assistant { tool_calls, .. } => {
                active_tool_call_ids.clear();
                if let Some(tool_calls) = tool_calls {
                    active_tool_call_ids.extend(tool_calls.iter().map(|call| call.id.as_str()));
                }
            }
            Message::Tool { tool_call_id, .. } => {
                if !active_tool_call_ids.contains(tool_call_id.as_str()) {
                    return Err(RewriteRejection::OrphanToolCallId {
                        tool_call_id: tool_call_id.clone(),
                    });
                }
            }
            Message::System { .. } | Message::User { .. } => {
                active_tool_call_ids.clear();
            }
        }
    }

    Ok(())
}

/// A handler subscribes to the events it cares about by overriding the matching
/// async methods. The default impls are no-ops, so a handler only needs to
/// implement the ones it uses.
#[async_trait::async_trait]
pub trait HookHandler: Send + Sync {
    fn name(&self) -> &str;

    /// Lower priority runs first. Default 50.
    fn priority(&self) -> i32 {
        50
    }

    async fn before_prompt(
        &self,
        _messages: &[Message],
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn on_turn_start(&self, _turn: u32, _cancel: CancellationToken) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn on_turn_end(&self, _turn: u32, _cancel: CancellationToken) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    /// GH issue #557: intercept raw user input before slash dispatch
    /// + turn execution. Outcomes honored: `Continue`, `BlockInput`,
    /// `ReplaceInput`. Default `Continue`.
    async fn on_input(&self, _raw: &str, _cancel: CancellationToken) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn on_message_start(
        &self,
        _delta: &StreamEvent,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn on_message_update(
        &self,
        _delta: &StreamEvent,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn on_message_end(
        &self,
        _delta: &StreamEvent,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn on_tool_execution_start(
        &self,
        _call: &ToolCall,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn on_tool_execution_update(
        &self,
        _call: &ToolCall,
        _progress: &ToolProgress,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn on_tool_execution_end(
        &self,
        _call: &ToolCall,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn on_context(
        &self,
        _messages: &[Message],
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn on_before_provider_request(
        &self,
        _messages: &[Message],
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn on_after_provider_response(
        &self,
        _response: &ChatResponse,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn on_session_before_compact(
        &self,
        _messages: &[Message],
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn on_session_compact(
        &self,
        _summary: &str,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn on_session_before_fork(&self, _cancel: CancellationToken) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn on_session_shutdown(&self, _cancel: CancellationToken) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn before_tool_call(
        &self,
        _tool: &str,
        _args: &Value,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn after_tool_call(
        &self,
        _tool: &str,
        _result: &ToolResult,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn before_agent_end(
        &self,
        _final_response: &str,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn session_start(&self, _session_id: &str) {}

    async fn session_end(&self, _session_id: &str, _total_turns: u32) {}
}

/// Snapshot of per-failure-mode counters. Returned from
/// [`HookRegistry::failure_metrics`] so callers (telemetry, tests) can
/// distinguish "handler crashed" from "handler too slow" without having
/// to grep tracing output (YYC-120).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HookFailureCounts {
    /// Handlers that exceeded `handler_timeout`.
    pub timeouts: usize,
    /// Handlers that returned `Err(_)` from their event method.
    pub errors: usize,
}

#[derive(Default)]
struct HookFailureMetrics {
    timeouts: AtomicUsize,
    errors: AtomicUsize,
}

impl HookFailureMetrics {
    fn snapshot(&self) -> HookFailureCounts {
        HookFailureCounts {
            timeouts: self.timeouts.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
        }
    }
}

/// Holds the registered handlers in priority order and exposes one emit method
/// per event.
pub struct HookRegistry {
    handlers: Vec<Arc<dyn HookHandler>>,
    handler_timeout: Duration,
    failure_metrics: HookFailureMetrics,
    /// GH issue #557: optional audit log. When set, `apply_on_input`
    /// records every non-Continue outcome as an `InputIntercept`
    /// event. `None` means audit silently skips (CLI one-shot path).
    audit_log: Option<Arc<crate::extensions::ExtensionAuditLog>>,
    /// Extension ids whose `ReplaceInput` outcomes need explicit user
    /// approval before the rewritten text is applied.
    input_rewrite_approval_required: HashSet<String>,
    /// Pause channel used to ask the active Frontend for approval.
    input_rewrite_pause_tx: Option<PauseSender>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
            audit_log: None,
            input_rewrite_approval_required: HashSet::new(),
            input_rewrite_pause_tx: None,
            handler_timeout: Duration::from_secs(30),
            failure_metrics: HookFailureMetrics::default(),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.handler_timeout = timeout;
        self
    }

    /// GH issue #557: install the daemon's `ExtensionAuditLog` so
    /// `apply_on_input` can record `InputIntercept` rows on Block /
    /// Replace outcomes. Optional — registries without an audit log
    /// silently skip recording.
    pub fn with_audit_log(mut self, log: Arc<crate::extensions::ExtensionAuditLog>) -> Self {
        self.audit_log = Some(log);
        self
    }

    pub fn with_input_rewrite_pause_channel(mut self, pause_tx: PauseSender) -> Self {
        self.input_rewrite_pause_tx = Some(pause_tx);
        self
    }

    pub fn require_input_rewrite_approval(mut self, extension_id: impl Into<String>) -> Self {
        self.input_rewrite_approval_required
            .insert(extension_id.into());
        self
    }

    pub fn mark_input_rewrite_approval_required(&mut self, extension_id: impl Into<String>) {
        self.input_rewrite_approval_required
            .insert(extension_id.into());
    }

    pub fn register(&mut self, handler: Arc<dyn HookHandler>) {
        self.handlers.push(handler);
        self.handlers.sort_by_key(|h| h.priority());
    }

    pub fn handler_count(&self) -> usize {
        self.handlers.len()
    }

    /// Snapshot of per-failure-mode counters since registry construction
    /// (YYC-120). Telemetry surfaces and tests use this to differentiate
    /// "handler crashed" from "handler too slow"; the agent loop logs each
    /// failure with the same distinction via tracing.
    pub fn failure_metrics(&self) -> HookFailureCounts {
        self.failure_metrics.snapshot()
    }

    /// Emit BeforePrompt to every handler and return the outgoing prompt with
    /// all injections applied at their requested positions. The input slice is
    /// not mutated — injections are transient and never persist into
    /// conversation history.
    pub async fn apply_before_prompt(
        &self,
        messages: &[Message],
        cancel: CancellationToken,
    ) -> Vec<Message> {
        self.apply_context(messages, cancel).await
    }

    /// Emit the wide `on_context` event plus legacy `before_prompt`
    /// compatibility hooks. `RewriteMessages` replaces the transient
    /// outgoing payload; `InjectMessages` accumulates around the current
    /// outgoing payload. Persistent history is never mutated.
    pub async fn apply_context(
        &self,
        messages: &[Message],
        cancel: CancellationToken,
    ) -> Vec<Message> {
        let mut current = messages.to_vec();
        let mut after_system: Vec<Message> = Vec::new();
        let mut appended: Vec<Message> = Vec::new();

        for h in &self.handlers {
            match self.run(h, h.on_context(&current, cancel.clone())).await {
                Some(HookOutcome::RewriteMessages(rewritten)) => {
                    tracing::info!("hook {} rewrote context messages", h.name());
                    current = rewritten;
                }
                Some(HookOutcome::InjectMessages {
                    messages: msgs,
                    position,
                }) => match position {
                    InjectPosition::AfterSystem => after_system.extend(msgs),
                    InjectPosition::Append => appended.extend(msgs),
                },
                Some(HookOutcome::Continue) | None => {}
                Some(other) => {
                    tracing::warn!(
                        "hook {} returned {:?} for on_context (ignored)",
                        h.name(),
                        other
                    );
                }
            }
            match self.run(h, h.before_prompt(messages, cancel.clone())).await {
                Some(HookOutcome::InjectMessages {
                    messages: msgs,
                    position,
                }) => match position {
                    InjectPosition::AfterSystem => after_system.extend(msgs),
                    InjectPosition::Append => appended.extend(msgs),
                },
                Some(HookOutcome::Continue) | None => {}
                Some(other) => {
                    tracing::warn!(
                        "hook {} returned {:?} for before_prompt (ignored)",
                        h.name(),
                        other
                    );
                }
            }
        }

        if after_system.is_empty() && appended.is_empty() {
            return current;
        }

        let cap = current.len() + after_system.len() + appended.len();
        let mut out: Vec<Message> = Vec::with_capacity(cap);
        let mut injected_after_system = false;
        for m in &current {
            if matches!(m, Message::System { .. }) {
                out.push(m.clone());
            } else {
                if !injected_after_system {
                    out.append(&mut after_system);
                    injected_after_system = true;
                }
                out.push(m.clone());
            }
        }
        if !injected_after_system {
            // No non-system message in the input — drop AfterSystem injections
            // at the tail of the system run.
            out.append(&mut after_system);
        }
        out.append(&mut appended);
        out
    }

    pub async fn on_turn_start(&self, turn: u32, cancel: CancellationToken) {
        for h in &self.handlers {
            match self.run(h, h.on_turn_start(turn, cancel.clone())).await {
                Some(HookOutcome::Continue) | None => {}
                Some(other) => {
                    tracing::warn!(
                        "hook {} returned {:?} for on_turn_start (ignored)",
                        h.name(),
                        other
                    );
                }
            }
        }
    }

    pub async fn on_turn_end(&self, turn: u32, cancel: CancellationToken) {
        for h in &self.handlers {
            match self.run(h, h.on_turn_end(turn, cancel.clone())).await {
                Some(HookOutcome::Continue) | None => {}
                Some(other) => {
                    tracing::warn!(
                        "hook {} returned {:?} for on_turn_end (ignored)",
                        h.name(),
                        other
                    );
                }
            }
        }
    }

    pub async fn on_message_start(&self, delta: &StreamEvent, cancel: CancellationToken) {
        for h in &self.handlers {
            self.ignore_observe_outcome(
                h,
                "on_message_start",
                self.run(h, h.on_message_start(delta, cancel.clone())).await,
            );
        }
    }

    pub async fn on_message_update(&self, delta: &StreamEvent, cancel: CancellationToken) {
        for h in &self.handlers {
            self.ignore_observe_outcome(
                h,
                "on_message_update",
                self.run(h, h.on_message_update(delta, cancel.clone()))
                    .await,
            );
        }
    }

    pub async fn on_message_end(&self, delta: &StreamEvent, cancel: CancellationToken) {
        for h in &self.handlers {
            self.ignore_observe_outcome(
                h,
                "on_message_end",
                self.run(h, h.on_message_end(delta, cancel.clone())).await,
            );
        }
    }

    pub async fn on_tool_execution_start(&self, call: &ToolCall, cancel: CancellationToken) {
        for h in &self.handlers {
            self.ignore_observe_outcome(
                h,
                "on_tool_execution_start",
                self.run(h, h.on_tool_execution_start(call, cancel.clone()))
                    .await,
            );
        }
    }

    pub async fn on_tool_execution_update(
        &self,
        call: &ToolCall,
        progress: &ToolProgress,
        cancel: CancellationToken,
    ) {
        for h in &self.handlers {
            self.ignore_observe_outcome(
                h,
                "on_tool_execution_update",
                self.run(
                    h,
                    h.on_tool_execution_update(call, progress, cancel.clone()),
                )
                .await,
            );
        }
    }

    pub async fn on_tool_execution_end(&self, call: &ToolCall, cancel: CancellationToken) {
        for h in &self.handlers {
            self.ignore_observe_outcome(
                h,
                "on_tool_execution_end",
                self.run(h, h.on_tool_execution_end(call, cancel.clone()))
                    .await,
            );
        }
    }

    pub async fn on_before_provider_request(
        &self,
        messages: &[Message],
        cancel: CancellationToken,
    ) {
        for h in &self.handlers {
            self.ignore_observe_outcome(
                h,
                "on_before_provider_request",
                self.run(h, h.on_before_provider_request(messages, cancel.clone()))
                    .await,
            );
        }
    }

    pub async fn on_after_provider_response(
        &self,
        response: &ChatResponse,
        cancel: CancellationToken,
    ) {
        for h in &self.handlers {
            self.ignore_observe_outcome(
                h,
                "on_after_provider_response",
                self.run(h, h.on_after_provider_response(response, cancel.clone()))
                    .await,
            );
        }
    }

    pub async fn on_session_before_compact(
        &self,
        messages: &[Message],
        cancel: CancellationToken,
    ) -> CompactionDecision {
        for h in &self.handlers {
            match self
                .run(h, h.on_session_before_compact(messages, cancel.clone()))
                .await
            {
                Some(HookOutcome::Block { reason }) => {
                    tracing::info!("hook {} blocked compaction: {reason}", h.name());
                    return CompactionDecision::Block {
                        extension_id: h.name().to_string(),
                        reason,
                    };
                }
                Some(HookOutcome::RewriteHistory(messages)) => {
                    tracing::info!("hook {} rewrote compaction history", h.name());
                    return CompactionDecision::RewriteHistory {
                        extension_id: h.name().to_string(),
                        messages,
                    };
                }
                Some(HookOutcome::Continue) | None => {}
                Some(other) => {
                    tracing::warn!(
                        "hook {} returned {:?} for on_session_before_compact (ignored)",
                        h.name(),
                        other
                    );
                }
            }
        }
        CompactionDecision::Continue
    }

    pub async fn on_session_compact(&self, summary: &str, cancel: CancellationToken) {
        for h in &self.handlers {
            self.ignore_observe_outcome(
                h,
                "on_session_compact",
                self.run(h, h.on_session_compact(summary, cancel.clone()))
                    .await,
            );
        }
    }

    pub async fn on_session_before_fork(&self, cancel: CancellationToken) {
        for h in &self.handlers {
            self.ignore_observe_outcome(
                h,
                "on_session_before_fork",
                self.run(h, h.on_session_before_fork(cancel.clone())).await,
            );
        }
    }

    pub async fn on_session_shutdown(&self, cancel: CancellationToken) {
        for h in &self.handlers {
            self.ignore_observe_outcome(
                h,
                "on_session_shutdown",
                self.run(h, h.on_session_shutdown(cancel.clone())).await,
            );
        }
    }

    /// GH issue #557: emit `on_input` to every handler. First
    /// non-Continue outcome wins. `BlockInput` short-circuits the
    /// prompt; `ReplaceInput` swaps the raw user text. Other outcome
    /// variants are logged + ignored. Block / Replace outcomes are
    /// also recorded to the registry's `ExtensionAuditLog` (when
    /// installed via `with_audit_log`), with the handler's `name()`
    /// as the audit `extension_id`.
    pub async fn apply_on_input(&self, raw: &str, cancel: CancellationToken) -> InputDecision {
        for h in &self.handlers {
            match self.run(h, h.on_input(raw, cancel.clone())).await {
                Some(HookOutcome::BlockInput { reason }) => {
                    tracing::info!("hook {} blocked input: {reason}", h.name());
                    self.record_input_intercept(
                        h.name(),
                        raw,
                        raw,
                        crate::extensions::InputInterceptAction::Block {
                            reason: reason.clone(),
                        },
                    );
                    return InputDecision::Block(reason);
                }
                Some(HookOutcome::ReplaceInput(rewrite)) => {
                    if let Err(reason) = self
                        .approve_input_rewrite_if_required(h.name(), raw, &rewrite)
                        .await
                    {
                        tracing::info!(
                            "hook {} input rewrite blocked before approval: {reason}",
                            h.name()
                        );
                        self.record_input_intercept(
                            h.name(),
                            raw,
                            raw,
                            crate::extensions::InputInterceptAction::Block {
                                reason: reason.clone(),
                            },
                        );
                        return InputDecision::Block(reason);
                    }
                    tracing::info!("hook {} replaced input", h.name());
                    self.record_input_intercept(
                        h.name(),
                        raw,
                        &rewrite,
                        crate::extensions::InputInterceptAction::Replace,
                    );
                    return InputDecision::Replace(rewrite);
                }
                Some(HookOutcome::Continue) | None => {}
                Some(other) => {
                    tracing::warn!(
                        "hook {} returned {:?} for on_input (ignored)",
                        h.name(),
                        other
                    );
                }
            }
        }
        InputDecision::Continue
    }

    async fn approve_input_rewrite_if_required(
        &self,
        extension_id: &str,
        before: &str,
        after: &str,
    ) -> std::result::Result<(), String> {
        if !self.input_rewrite_approval_required.contains(extension_id) {
            return Ok(());
        }
        let Some(tx) = &self.input_rewrite_pause_tx else {
            return Err(format!(
                "extension `{extension_id}` requires user approval for input rewrite but no pause channel is wired"
            ));
        };
        let (reply, rx) = tokio::sync::oneshot::channel();
        let pause = AgentPause {
            kind: PauseKind::InputRewriteApproval {
                extension_id: extension_id.to_string(),
                before: before.to_string(),
                after: after.to_string(),
            },
            reply,
            options: vec![
                PauseOption {
                    key: 'a',
                    label: "allow once".to_string(),
                    kind: OptionKind::Primary,
                    resume: AgentResume::Allow,
                },
                PauseOption {
                    key: 'd',
                    label: "deny".to_string(),
                    kind: OptionKind::Destructive,
                    resume: AgentResume::DenyWithReason("user denied input rewrite".to_string()),
                },
            ],
        };
        if tx.send(pause).await.is_err() {
            return Err(format!(
                "extension `{extension_id}` requires user approval for input rewrite but pause consumer dropped"
            ));
        }
        match rx.await {
            Ok(AgentResume::Allow | AgentResume::AllowAndRemember) => Ok(()),
            Ok(AgentResume::Deny) => Err(format!(
                "input rewrite denied for extension `{extension_id}`"
            )),
            Ok(AgentResume::DenyWithReason(reason)) => Err(format!(
                "input rewrite denied for extension `{extension_id}`: {reason}"
            )),
            Ok(other) => Err(format!(
                "input rewrite denied for extension `{extension_id}`: unsupported resume {other:?}"
            )),
            Err(_) => Err(format!(
                "extension `{extension_id}` requires user approval for input rewrite but pause reply was dropped"
            )),
        }
    }

    fn record_input_intercept(
        &self,
        extension_id: &str,
        before: &str,
        after: &str,
        action: crate::extensions::InputInterceptAction,
    ) {
        let Some(log) = self.audit_log.as_ref() else {
            return;
        };
        log.record(crate::extensions::ExtensionAuditEvent::InputIntercept(
            crate::extensions::InputInterceptEvent {
                extension_id: extension_id.to_string(),
                before: before.to_string(),
                after: after.to_string(),
                action,
                occurred_at: chrono::Utc::now(),
            },
        ));
    }

    pub(crate) fn record_compaction_validation_failed(
        &self,
        extension_id: &str,
        rejection: RewriteRejection,
    ) {
        self.record_compaction_event(
            extension_id,
            crate::extensions::CompactionAuditAction::ValidationFailed { rejection },
        );
    }

    pub(crate) fn record_compaction_forced(&self, extension_id: &str, reason: &str) {
        self.record_compaction_event(
            extension_id,
            crate::extensions::CompactionAuditAction::Forced {
                reason: reason.to_string(),
            },
        );
    }

    fn record_compaction_event(
        &self,
        extension_id: &str,
        action: crate::extensions::CompactionAuditAction,
    ) {
        let Some(log) = self.audit_log.as_ref() else {
            return;
        };
        log.record(crate::extensions::ExtensionAuditEvent::Compaction(
            crate::extensions::CompactionAuditEvent {
                extension_id: extension_id.to_string(),
                action,
                occurred_at: chrono::Utc::now(),
            },
        ));
    }

    /// Emit BeforeToolCall. First non-Continue outcome wins.
    pub async fn before_tool_call(
        &self,
        tool: &str,
        args: &Value,
        cancel: CancellationToken,
    ) -> ToolCallDecision {
        for h in &self.handlers {
            match self
                .run(h, h.before_tool_call(tool, args, cancel.clone()))
                .await
            {
                Some(HookOutcome::Block { reason }) => {
                    tracing::info!("hook {} blocked tool {tool}: {reason}", h.name());
                    return ToolCallDecision::Block(reason);
                }
                Some(HookOutcome::ReplaceArgs(new_args)) => {
                    tracing::info!("hook {} replaced args for {tool}", h.name());
                    return ToolCallDecision::ReplaceArgs(new_args);
                }
                Some(HookOutcome::Continue) | None => {}
                Some(other) => {
                    tracing::warn!(
                        "hook {} returned {:?} for before_tool_call (ignored)",
                        h.name(),
                        other
                    );
                }
            }
        }
        ToolCallDecision::Continue
    }

    /// Emit AfterToolCall. First ReplaceResult wins; otherwise None.
    pub async fn after_tool_call(
        &self,
        tool: &str,
        result: &ToolResult,
        cancel: CancellationToken,
    ) -> Option<ToolResult> {
        for h in &self.handlers {
            match self
                .run(h, h.after_tool_call(tool, result, cancel.clone()))
                .await
            {
                Some(HookOutcome::ReplaceResult(new)) => {
                    tracing::info!("hook {} replaced result for {tool}", h.name());
                    return Some(new);
                }
                Some(HookOutcome::Continue) | None => {}
                Some(other) => {
                    tracing::warn!(
                        "hook {} returned {:?} for after_tool_call (ignored)",
                        h.name(),
                        other
                    );
                }
            }
        }
        None
    }

    /// Emit BeforeAgentEnd. First ForceContinue wins; returned instruction is
    /// appended as a user turn and the loop continues.
    pub async fn before_agent_end(
        &self,
        response: &str,
        cancel: CancellationToken,
    ) -> Option<String> {
        for h in &self.handlers {
            match self
                .run(h, h.before_agent_end(response, cancel.clone()))
                .await
            {
                Some(HookOutcome::ForceContinue { instruction }) => {
                    tracing::info!("hook {} forced continue", h.name());
                    return Some(instruction);
                }
                Some(HookOutcome::Continue) | None => {}
                Some(other) => {
                    tracing::warn!(
                        "hook {} returned {:?} for before_agent_end (ignored)",
                        h.name(),
                        other
                    );
                }
            }
        }
        None
    }

    pub async fn session_start(&self, session_id: &str) {
        for h in &self.handlers {
            // Each handler gets its own timeout window. session_X are observe-
            // only so we don't care about return values.
            let _ = timeout(self.handler_timeout, h.session_start(session_id)).await;
        }
    }

    pub async fn session_end(&self, session_id: &str, total_turns: u32) {
        for h in &self.handlers {
            let _ = timeout(self.handler_timeout, h.session_end(session_id, total_turns)).await;
        }
    }

    async fn run<F>(&self, h: &Arc<dyn HookHandler>, fut: F) -> Option<HookOutcome>
    where
        F: std::future::Future<Output = Result<HookOutcome>>,
    {
        match timeout(self.handler_timeout, fut).await {
            Ok(Ok(o)) => Some(o),
            Ok(Err(e)) => {
                // YYC-120: errors and timeouts both drop the handler's
                // contribution to the event, but they're operationally
                // distinct — a crashed handler is a bug, a slow one is a
                // capacity / dependency problem. Count them separately so
                // metrics surfaces (and `failure_metrics()`) can branch on
                // the failure mode rather than just "something went wrong".
                self.failure_metrics.errors.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    handler = h.name(),
                    failure = "error",
                    "hook {} returned error: {e}",
                    h.name()
                );
                None
            }
            Err(_) => {
                self.failure_metrics
                    .timeouts
                    .fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    handler = h.name(),
                    failure = "timeout",
                    timeout_ms = self.handler_timeout.as_millis() as u64,
                    "hook {} timed out after {:?}",
                    h.name(),
                    self.handler_timeout
                );
                None
            }
        }
    }

    fn ignore_observe_outcome(
        &self,
        h: &Arc<dyn HookHandler>,
        event: &'static str,
        outcome: Option<HookOutcome>,
    ) {
        match outcome {
            Some(HookOutcome::Continue) | None => {}
            Some(other) => {
                tracing::warn!(
                    "hook {} returned {:?} for {} (ignored)",
                    h.name(),
                    other,
                    event
                );
            }
        }
    }
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Test handler with configurable behavior. Records how many times each
    /// event fired so tests can assert isolation.
    struct Probe {
        name: &'static str,
        priority: i32,
        before_tool_outcome: HookOutcome,
        before_tool_calls: AtomicUsize,
        sleep_ms: u64,
        return_error: bool,
    }

    impl Probe {
        fn new(name: &'static str, priority: i32, outcome: HookOutcome) -> Self {
            Self {
                name,
                priority,
                before_tool_outcome: outcome,
                before_tool_calls: AtomicUsize::new(0),
                sleep_ms: 0,
                return_error: false,
            }
        }
        fn slow(mut self, ms: u64) -> Self {
            self.sleep_ms = ms;
            self
        }
        /// Force the handler to return `Err(_)` from `before_tool_call`.
        /// Used by YYC-120 tests to exercise the error vs. timeout split.
        fn errors(mut self) -> Self {
            self.return_error = true;
            self
        }
    }

    #[async_trait::async_trait]
    impl HookHandler for Probe {
        fn name(&self) -> &str {
            self.name
        }
        fn priority(&self) -> i32 {
            self.priority
        }
        async fn before_tool_call(
            &self,
            _tool: &str,
            _args: &Value,
            _cancel: CancellationToken,
        ) -> Result<HookOutcome> {
            self.before_tool_calls.fetch_add(1, Ordering::SeqCst);
            if self.sleep_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.sleep_ms)).await;
            }
            if self.return_error {
                anyhow::bail!("synthetic handler failure");
            }
            Ok(self.before_tool_outcome.clone())
        }
    }

    #[tokio::test]
    async fn first_block_wins_and_short_circuits_subsequent_handlers() {
        let mut reg = HookRegistry::new();
        let blocker = Arc::new(Probe::new(
            "blocker",
            10,
            HookOutcome::Block {
                reason: "nope".into(),
            },
        ));
        let after = Arc::new(Probe::new("after", 20, HookOutcome::Continue));
        reg.register(blocker.clone());
        reg.register(after.clone());

        let decision = reg
            .before_tool_call("bash", &Value::Null, CancellationToken::new())
            .await;

        match decision {
            ToolCallDecision::Block(reason) => assert_eq!(reason, "nope"),
            other => panic!("expected Block, got {other:?}"),
        }

        // Earlier handler fired, later one was short-circuited.
        assert_eq!(blocker.before_tool_calls.load(Ordering::SeqCst), 1);
        assert_eq!(after.before_tool_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn priority_ordering_lower_runs_first() {
        let mut reg = HookRegistry::new();
        let probe_a = Arc::new(Probe::new(
            "a",
            1,
            HookOutcome::Block {
                reason: "first".into(),
            },
        ));
        let probe_b = Arc::new(Probe::new(
            "b",
            50,
            HookOutcome::Block {
                reason: "second".into(),
            },
        ));
        // Register b BEFORE a; sort should still pick a.
        reg.register(probe_b.clone());
        reg.register(probe_a.clone());

        let decision = reg
            .before_tool_call("bash", &Value::Null, CancellationToken::new())
            .await;
        match decision {
            ToolCallDecision::Block(r) => assert_eq!(r, "first"),
            other => panic!("expected first probe to win, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn timeout_and_error_increment_distinct_failure_counters() {
        // YYC-120: production observability needs a clear split between
        // "handler too slow" and "handler crashed". Both still drop the
        // outcome (the loop is unaffected), but the counters differ so
        // metrics surfaces can branch on the failure mode.
        let mut reg = HookRegistry::new().with_timeout(std::time::Duration::from_millis(50));
        let slow = Arc::new(Probe::new("slow", 10, HookOutcome::Continue).slow(500));
        let crashed = Arc::new(Probe::new("crashed", 20, HookOutcome::Continue).errors());
        reg.register(slow);
        reg.register(crashed);

        let _ = reg
            .before_tool_call("bash", &Value::Null, CancellationToken::new())
            .await;

        let counts = reg.failure_metrics();
        assert_eq!(
            counts.timeouts, 1,
            "timeout should bump the timeout counter"
        );
        assert_eq!(counts.errors, 1, "error should bump the error counter");

        // Second invocation accumulates rather than resets.
        let _ = reg
            .before_tool_call("bash", &Value::Null, CancellationToken::new())
            .await;
        let counts = reg.failure_metrics();
        assert_eq!(counts.timeouts, 2);
        assert_eq!(counts.errors, 2);
    }

    #[tokio::test]
    async fn handler_timeout_does_not_break_loop() {
        let mut reg = HookRegistry::new().with_timeout(std::time::Duration::from_millis(50));
        // First handler sleeps past the timeout window.
        let slow = Arc::new(
            Probe::new(
                "slow",
                10,
                HookOutcome::Block {
                    reason: "would block".into(),
                },
            )
            .slow(500),
        );
        // Second handler is fast and would Continue.
        let fast = Arc::new(Probe::new("fast", 20, HookOutcome::Continue));
        reg.register(slow.clone());
        reg.register(fast.clone());

        let decision = reg
            .before_tool_call("bash", &Value::Null, CancellationToken::new())
            .await;

        // Slow handler timed out → its Block outcome is dropped → fast handler
        // runs → Continue.
        assert!(matches!(decision, ToolCallDecision::Continue));
        assert_eq!(fast.before_tool_calls.load(Ordering::SeqCst), 1);
    }

    /// Hook that injects a System message via BeforePrompt.
    struct Injector {
        name: &'static str,
        msg: String,
        position: InjectPosition,
    }

    #[async_trait::async_trait]
    impl HookHandler for Injector {
        fn name(&self) -> &str {
            self.name
        }
        async fn before_prompt(
            &self,
            _messages: &[Message],
            _cancel: CancellationToken,
        ) -> Result<HookOutcome> {
            Ok(HookOutcome::InjectMessages {
                messages: vec![Message::System {
                    content: self.msg.clone(),
                }],
                position: self.position,
            })
        }
    }

    #[tokio::test]
    async fn before_prompt_injections_accumulate_and_position() {
        let mut reg = HookRegistry::new();
        reg.register(Arc::new(Injector {
            name: "after-system-1",
            msg: "AS1".into(),
            position: InjectPosition::AfterSystem,
        }));
        reg.register(Arc::new(Injector {
            name: "after-system-2",
            msg: "AS2".into(),
            position: InjectPosition::AfterSystem,
        }));
        reg.register(Arc::new(Injector {
            name: "appended",
            msg: "TAIL".into(),
            position: InjectPosition::Append,
        }));

        let input = vec![
            Message::System {
                content: "you are agent".into(),
            },
            Message::User {
                content: "hi".into(),
            },
        ];
        let outgoing = reg
            .apply_before_prompt(&input, CancellationToken::new())
            .await;

        // Expected order: System(original), System(AS1), System(AS2), User, System(TAIL).
        assert_eq!(outgoing.len(), 5);
        match &outgoing[0] {
            Message::System { content } => assert_eq!(content, "you are agent"),
            o => panic!("expected original system, got {o:?}"),
        }
        match &outgoing[1] {
            Message::System { content } => assert_eq!(content, "AS1"),
            o => panic!("expected AS1, got {o:?}"),
        }
        match &outgoing[2] {
            Message::System { content } => assert_eq!(content, "AS2"),
            o => panic!("expected AS2, got {o:?}"),
        }
        match &outgoing[3] {
            Message::User { content } => assert_eq!(content, "hi"),
            o => panic!("expected user, got {o:?}"),
        }
        match &outgoing[4] {
            Message::System { content } => assert_eq!(content, "TAIL"),
            o => panic!("expected appended TAIL, got {o:?}"),
        }
    }

    #[test]
    fn new_hook_outcomes_round_trip_through_serde() {
        let rewrite = HookOutcome::RewriteMessages(vec![Message::User {
            content: "rewritten".into(),
        }]);
        let encoded = serde_json::to_string(&rewrite).expect("serialize rewrite");
        let decoded: HookOutcome = serde_json::from_str(&encoded).expect("deserialize rewrite");
        assert!(matches!(
            decoded,
            HookOutcome::RewriteMessages(messages)
                if matches!(messages.as_slice(), [Message::User { content }] if content == "rewritten")
        ));

        let blocked = HookOutcome::BlockInput {
            reason: "policy".into(),
        };
        let encoded = serde_json::to_string(&blocked).expect("serialize block input");
        let decoded: HookOutcome = serde_json::from_str(&encoded).expect("deserialize block input");
        assert!(matches!(
            decoded,
            HookOutcome::BlockInput { reason } if reason == "policy"
        ));

        let replaced = HookOutcome::ReplaceInput("expanded".into());
        let encoded = serde_json::to_string(&replaced).expect("serialize replace input");
        let decoded: HookOutcome =
            serde_json::from_str(&encoded).expect("deserialize replace input");
        assert!(matches!(decoded, HookOutcome::ReplaceInput(text) if text == "expanded"));
    }

    fn assistant_with_tool_call(id: &str) -> Message {
        Message::Assistant {
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: id.into(),
                call_type: "function".into(),
                function: crate::provider::ToolCallFunction {
                    name: "noop".into(),
                    arguments: "{}".into(),
                },
            }]),
            reasoning_content: None,
        }
    }

    fn tool_result_message(id: &str) -> Message {
        Message::Tool {
            tool_call_id: id.into(),
            content: "ok".into(),
        }
    }

    #[test]
    fn rewrite_history_validator_rejects_missing_system() {
        let input = vec![
            Message::System {
                content: "system".into(),
            },
            Message::User {
                content: "old".into(),
            },
        ];
        let proposed = vec![Message::User {
            content: "summary".into(),
        }];

        assert!(matches!(
            validate_rewrite_history(&input, &proposed),
            Err(RewriteRejection::MissingSystem)
        ));
    }

    #[test]
    fn rewrite_history_validator_rejects_orphan_tool_call_id() {
        let input = vec![
            Message::System {
                content: "system".into(),
            },
            Message::User {
                content: "old".into(),
            },
            assistant_with_tool_call("call_1"),
            tool_result_message("call_1"),
        ];
        let proposed = vec![
            Message::System {
                content: "system".into(),
            },
            tool_result_message("missing"),
        ];

        assert!(matches!(
            validate_rewrite_history(&input, &proposed),
            Err(RewriteRejection::OrphanToolCallId { tool_call_id }) if tool_call_id == "missing"
        ));
    }

    #[test]
    fn rewrite_history_validator_rejects_non_shrinking_history() {
        let input = vec![
            Message::System {
                content: "system".into(),
            },
            Message::User {
                content: "old".into(),
            },
        ];
        let proposed = input.clone();

        assert!(matches!(
            validate_rewrite_history(&input, &proposed),
            Err(RewriteRejection::NotShorter {
                input_len: 2,
                proposed_len: 2
            })
        ));
    }

    #[test]
    fn rewrite_history_validator_accepts_valid_tool_history() {
        let input = vec![
            Message::System {
                content: "system".into(),
            },
            Message::User {
                content: "old".into(),
            },
            assistant_with_tool_call("call_1"),
            tool_result_message("call_1"),
        ];
        let proposed = vec![
            Message::System {
                content: "system".into(),
            },
            assistant_with_tool_call("call_1"),
            tool_result_message("call_1"),
        ];

        assert!(validate_rewrite_history(&input, &proposed).is_ok());
    }

    struct ContextRewriter;

    #[async_trait::async_trait]
    impl HookHandler for ContextRewriter {
        fn name(&self) -> &str {
            "context-rewriter"
        }

        async fn on_context(
            &self,
            _messages: &[Message],
            _cancel: CancellationToken,
        ) -> Result<HookOutcome> {
            Ok(HookOutcome::RewriteMessages(vec![
                Message::System {
                    content: "rewritten system".into(),
                },
                Message::User {
                    content: "rewritten user".into(),
                },
            ]))
        }
    }

    #[tokio::test]
    async fn on_context_rewrite_messages_replaces_outgoing_prompt_transiently() {
        let mut reg = HookRegistry::new();
        reg.register(Arc::new(ContextRewriter));
        let input = vec![Message::User {
            content: "original".into(),
        }];

        let outgoing = reg.apply_context(&input, CancellationToken::new()).await;

        assert_eq!(outgoing.len(), 2);
        assert!(matches!(
            &outgoing[0],
            Message::System { content } if content == "rewritten system"
        ));
        assert!(matches!(
            &outgoing[1],
            Message::User { content } if content == "rewritten user"
        ));
        assert!(matches!(
            &input[0],
            Message::User { content } if content == "original"
        ));
    }

    #[tokio::test]
    async fn on_turn_start_timeouts_are_isolated_and_counted() {
        struct SlowTurnStart;

        #[async_trait::async_trait]
        impl HookHandler for SlowTurnStart {
            fn name(&self) -> &str {
                "slow-turn-start"
            }

            async fn on_turn_start(
                &self,
                _turn: u32,
                _cancel: CancellationToken,
            ) -> Result<HookOutcome> {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                Ok(HookOutcome::Continue)
            }
        }

        let mut reg = HookRegistry::new().with_timeout(std::time::Duration::from_millis(10));
        reg.register(Arc::new(SlowTurnStart));

        reg.on_turn_start(1, CancellationToken::new()).await;

        assert_eq!(reg.failure_metrics().timeouts, 1);
    }

    fn sample_tool_call() -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            call_type: "function".into(),
            function: crate::provider::ToolCallFunction {
                name: "probe".into(),
                arguments: "{}".into(),
            },
        }
    }

    fn sample_chat_response() -> ChatResponse {
        ChatResponse {
            content: Some("done".into()),
            tool_calls: None,
            usage: None,
            finish_reason: Some("stop".into()),
            reasoning_content: None,
        }
    }

    struct WideRecorder {
        name: &'static str,
        priority: i32,
        events: Arc<Mutex<Vec<String>>>,
    }

    impl WideRecorder {
        fn push(&self, event: &'static str) {
            self.events
                .lock()
                .unwrap()
                .push(format!("{}:{event}", self.name));
        }
    }

    #[async_trait::async_trait]
    impl HookHandler for WideRecorder {
        fn name(&self) -> &str {
            self.name
        }

        fn priority(&self) -> i32 {
            self.priority
        }

        async fn on_turn_start(
            &self,
            _turn: u32,
            _cancel: CancellationToken,
        ) -> Result<HookOutcome> {
            self.push("turn_start");
            Ok(HookOutcome::Continue)
        }

        async fn on_turn_end(&self, _turn: u32, _cancel: CancellationToken) -> Result<HookOutcome> {
            self.push("turn_end");
            Ok(HookOutcome::Continue)
        }

        async fn on_message_start(
            &self,
            _delta: &StreamEvent,
            _cancel: CancellationToken,
        ) -> Result<HookOutcome> {
            self.push("message_start");
            Ok(HookOutcome::Continue)
        }

        async fn on_message_update(
            &self,
            _delta: &StreamEvent,
            _cancel: CancellationToken,
        ) -> Result<HookOutcome> {
            self.push("message_update");
            Ok(HookOutcome::Continue)
        }

        async fn on_message_end(
            &self,
            _delta: &StreamEvent,
            _cancel: CancellationToken,
        ) -> Result<HookOutcome> {
            self.push("message_end");
            Ok(HookOutcome::Continue)
        }

        async fn on_tool_execution_start(
            &self,
            _call: &ToolCall,
            _cancel: CancellationToken,
        ) -> Result<HookOutcome> {
            self.push("tool_start");
            Ok(HookOutcome::Continue)
        }

        async fn on_tool_execution_update(
            &self,
            _call: &ToolCall,
            _progress: &ToolProgress,
            _cancel: CancellationToken,
        ) -> Result<HookOutcome> {
            self.push("tool_update");
            Ok(HookOutcome::Continue)
        }

        async fn on_tool_execution_end(
            &self,
            _call: &ToolCall,
            _cancel: CancellationToken,
        ) -> Result<HookOutcome> {
            self.push("tool_end");
            Ok(HookOutcome::Continue)
        }

        async fn on_before_provider_request(
            &self,
            _messages: &[Message],
            _cancel: CancellationToken,
        ) -> Result<HookOutcome> {
            self.push("provider_before");
            Ok(HookOutcome::Continue)
        }

        async fn on_after_provider_response(
            &self,
            _response: &ChatResponse,
            _cancel: CancellationToken,
        ) -> Result<HookOutcome> {
            self.push("provider_after");
            Ok(HookOutcome::Continue)
        }

        async fn on_session_before_compact(
            &self,
            _messages: &[Message],
            _cancel: CancellationToken,
        ) -> Result<HookOutcome> {
            self.push("session_before_compact");
            Ok(HookOutcome::Continue)
        }

        async fn on_session_compact(
            &self,
            _summary: &str,
            _cancel: CancellationToken,
        ) -> Result<HookOutcome> {
            self.push("session_compact");
            Ok(HookOutcome::Continue)
        }

        async fn on_session_before_fork(&self, _cancel: CancellationToken) -> Result<HookOutcome> {
            self.push("session_before_fork");
            Ok(HookOutcome::Continue)
        }

        async fn on_session_shutdown(&self, _cancel: CancellationToken) -> Result<HookOutcome> {
            self.push("session_shutdown");
            Ok(HookOutcome::Continue)
        }
    }

    #[tokio::test]
    async fn wide_observe_events_run_in_priority_order() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut reg = HookRegistry::new();
        reg.register(Arc::new(WideRecorder {
            name: "second",
            priority: 20,
            events: events.clone(),
        }));
        reg.register(Arc::new(WideRecorder {
            name: "first",
            priority: 10,
            events: events.clone(),
        }));

        let cancel = CancellationToken::new();
        let message = StreamEvent::Text("chunk".into());
        let call = sample_tool_call();
        let response = sample_chat_response();
        let messages = vec![Message::User {
            content: "hello".into(),
        }];

        reg.on_turn_start(1, cancel.clone()).await;
        reg.on_turn_end(1, cancel.clone()).await;
        reg.on_message_start(&message, cancel.clone()).await;
        reg.on_message_update(&message, cancel.clone()).await;
        reg.on_message_end(&message, cancel.clone()).await;
        reg.on_tool_execution_start(&call, cancel.clone()).await;
        reg.on_tool_execution_update(
            &call,
            &ToolProgress {
                message: "halfway".into(),
            },
            cancel.clone(),
        )
        .await;
        reg.on_tool_execution_end(&call, cancel.clone()).await;
        reg.on_before_provider_request(&messages, cancel.clone())
            .await;
        reg.on_after_provider_response(&response, cancel.clone())
            .await;
        assert!(matches!(
            reg.on_session_before_compact(&messages, cancel.clone())
                .await,
            CompactionDecision::Continue
        ));
        reg.on_session_compact("summary", cancel.clone()).await;
        reg.on_session_before_fork(cancel.clone()).await;
        reg.on_session_shutdown(cancel).await;

        let events = events.lock().unwrap().clone();
        for pair in events.chunks_exact(2) {
            assert_eq!(pair[0].split(':').next(), Some("first"));
            assert_eq!(pair[1].split(':').next(), Some("second"));
            assert_eq!(pair[0].split(':').nth(1), pair[1].split(':').nth(1));
        }
    }

    #[tokio::test]
    async fn wide_observe_event_timeouts_are_isolated() {
        struct SlowMessageUpdate;
        struct FastMessageUpdate(Arc<AtomicUsize>);

        #[async_trait::async_trait]
        impl HookHandler for SlowMessageUpdate {
            fn name(&self) -> &str {
                "slow-message-update"
            }

            fn priority(&self) -> i32 {
                10
            }

            async fn on_message_update(
                &self,
                _delta: &StreamEvent,
                _cancel: CancellationToken,
            ) -> Result<HookOutcome> {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                Ok(HookOutcome::Continue)
            }
        }

        #[async_trait::async_trait]
        impl HookHandler for FastMessageUpdate {
            fn name(&self) -> &str {
                "fast-message-update"
            }

            fn priority(&self) -> i32 {
                20
            }

            async fn on_message_update(
                &self,
                _delta: &StreamEvent,
                _cancel: CancellationToken,
            ) -> Result<HookOutcome> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(HookOutcome::Continue)
            }
        }

        let fast_calls = Arc::new(AtomicUsize::new(0));
        let mut reg = HookRegistry::new().with_timeout(std::time::Duration::from_millis(10));
        reg.register(Arc::new(SlowMessageUpdate));
        reg.register(Arc::new(FastMessageUpdate(fast_calls.clone())));

        reg.on_message_update(&StreamEvent::Text("chunk".into()), CancellationToken::new())
            .await;

        assert_eq!(reg.failure_metrics().timeouts, 1);
        assert_eq!(fast_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn session_before_compact_block_short_circuits_later_handlers() {
        struct BlockingCompact;
        struct LaterCompact(Arc<AtomicUsize>);

        #[async_trait::async_trait]
        impl HookHandler for BlockingCompact {
            fn name(&self) -> &str {
                "blocking-compact"
            }

            fn priority(&self) -> i32 {
                10
            }

            async fn on_session_before_compact(
                &self,
                _messages: &[Message],
                _cancel: CancellationToken,
            ) -> Result<HookOutcome> {
                Ok(HookOutcome::Block {
                    reason: "skip".into(),
                })
            }
        }

        #[async_trait::async_trait]
        impl HookHandler for LaterCompact {
            fn name(&self) -> &str {
                "later-compact"
            }

            fn priority(&self) -> i32 {
                20
            }

            async fn on_session_before_compact(
                &self,
                _messages: &[Message],
                _cancel: CancellationToken,
            ) -> Result<HookOutcome> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(HookOutcome::Continue)
            }
        }

        let later_calls = Arc::new(AtomicUsize::new(0));
        let mut reg = HookRegistry::new();
        reg.register(Arc::new(BlockingCompact));
        reg.register(Arc::new(LaterCompact(later_calls.clone())));

        assert!(matches!(
            reg.on_session_before_compact(&[], CancellationToken::new())
                .await,
            CompactionDecision::Block {
                extension_id,
                reason
            } if extension_id == "blocking-compact" && reason == "skip"
        ));
        assert_eq!(later_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn session_before_compact_returns_first_rewrite_history() {
        struct Rewriter;

        #[async_trait::async_trait]
        impl HookHandler for Rewriter {
            fn name(&self) -> &str {
                "compact-summary"
            }

            async fn on_session_before_compact(
                &self,
                messages: &[Message],
                _cancel: CancellationToken,
            ) -> Result<HookOutcome> {
                Ok(HookOutcome::RewriteHistory(vec![messages[0].clone()]))
            }
        }

        let messages = vec![
            Message::System {
                content: "system".into(),
            },
            Message::User {
                content: "old".into(),
            },
        ];
        let mut reg = HookRegistry::new();
        reg.register(Arc::new(Rewriter));

        assert!(matches!(
            reg.on_session_before_compact(&messages, CancellationToken::new())
                .await,
            CompactionDecision::RewriteHistory {
                extension_id,
                messages
            } if extension_id == "compact-summary" && messages.len() == 1
        ));
    }

    #[tokio::test]
    async fn apply_on_input_records_audit_event_for_replace() {
        use crate::extensions::{ExtensionAuditEvent, ExtensionAuditLog, InputInterceptAction};

        struct Rewriter;

        #[async_trait::async_trait]
        impl HookHandler for Rewriter {
            fn name(&self) -> &str {
                "input-demo"
            }
            async fn on_input(
                &self,
                _raw: &str,
                _cancel: CancellationToken,
            ) -> Result<HookOutcome> {
                Ok(HookOutcome::ReplaceInput("rewritten".into()))
            }
        }

        let audit = Arc::new(ExtensionAuditLog::new(8));
        let mut reg = HookRegistry::new().with_audit_log(audit.clone());
        reg.register(Arc::new(Rewriter));

        let _ = reg.apply_on_input("raw", CancellationToken::new()).await;

        let recent = audit.recent(1);
        assert_eq!(recent.len(), 1);
        match &recent[0] {
            ExtensionAuditEvent::InputIntercept(e) => {
                assert_eq!(e.extension_id, "input-demo");
                assert_eq!(e.before, "raw");
                assert_eq!(e.after, "rewritten");
                assert!(matches!(e.action, InputInterceptAction::Replace));
            }
            other => panic!("expected InputIntercept, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn apply_on_input_records_audit_event_for_block() {
        use crate::extensions::{ExtensionAuditEvent, ExtensionAuditLog, InputInterceptAction};

        struct Blocker;

        #[async_trait::async_trait]
        impl HookHandler for Blocker {
            fn name(&self) -> &str {
                "input-policy"
            }
            async fn on_input(
                &self,
                _raw: &str,
                _cancel: CancellationToken,
            ) -> Result<HookOutcome> {
                Ok(HookOutcome::BlockInput {
                    reason: "denied".into(),
                })
            }
        }

        let audit = Arc::new(ExtensionAuditLog::new(8));
        let mut reg = HookRegistry::new().with_audit_log(audit.clone());
        reg.register(Arc::new(Blocker));

        let _ = reg.apply_on_input("hello", CancellationToken::new()).await;

        let recent = audit.recent(1);
        match &recent[0] {
            ExtensionAuditEvent::InputIntercept(e) => {
                assert_eq!(e.extension_id, "input-policy");
                assert_eq!(e.before, "hello");
                assert_eq!(e.after, "hello");
                match &e.action {
                    InputInterceptAction::Block { reason } => assert_eq!(reason, "denied"),
                    _ => panic!("expected Block action"),
                }
            }
            other => panic!("expected InputIntercept, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn apply_on_input_does_not_record_audit_when_continue() {
        use crate::extensions::ExtensionAuditLog;

        struct Noop;

        #[async_trait::async_trait]
        impl HookHandler for Noop {
            fn name(&self) -> &str {
                "noop"
            }
        }

        let audit = Arc::new(ExtensionAuditLog::new(8));
        let mut reg = HookRegistry::new().with_audit_log(audit.clone());
        reg.register(Arc::new(Noop));

        let _ = reg.apply_on_input("hello", CancellationToken::new()).await;

        assert_eq!(audit.len(), 0);
    }

    #[tokio::test]
    async fn apply_on_input_returns_replace_when_hook_returns_replace_input() {
        struct Rewriter;

        #[async_trait::async_trait]
        impl HookHandler for Rewriter {
            fn name(&self) -> &str {
                "rewriter"
            }
            async fn on_input(
                &self,
                _raw: &str,
                _cancel: CancellationToken,
            ) -> Result<HookOutcome> {
                Ok(HookOutcome::ReplaceInput("rewritten".into()))
            }
        }

        let mut reg = HookRegistry::new();
        reg.register(Arc::new(Rewriter));

        let decision = reg
            .apply_on_input("original", CancellationToken::new())
            .await;

        match decision {
            InputDecision::Replace(text) => assert_eq!(text, "rewritten"),
            other => panic!("expected Replace, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn apply_on_input_allows_rewrite_when_approval_required_and_user_allows() {
        use crate::pause::{AgentResume, PauseKind};

        struct Rewriter;

        #[async_trait::async_trait]
        impl HookHandler for Rewriter {
            fn name(&self) -> &str {
                "approval-rewriter"
            }
            async fn on_input(
                &self,
                _raw: &str,
                _cancel: CancellationToken,
            ) -> Result<HookOutcome> {
                Ok(HookOutcome::ReplaceInput("rewritten".into()))
            }
        }

        let (pause_tx, mut pause_rx) = crate::pause::channel(4);
        let mut reg = HookRegistry::new()
            .with_input_rewrite_pause_channel(pause_tx)
            .require_input_rewrite_approval("approval-rewriter");
        reg.register(Arc::new(Rewriter));

        let decision_task =
            tokio::spawn(async move { reg.apply_on_input("raw", CancellationToken::new()).await });
        let pause = pause_rx.recv().await.expect("approval pause should emit");
        match &pause.kind {
            PauseKind::InputRewriteApproval {
                extension_id,
                before,
                after,
            } => {
                assert_eq!(extension_id, "approval-rewriter");
                assert_eq!(before, "raw");
                assert_eq!(after, "rewritten");
            }
            other => panic!("expected input rewrite approval, got {other:?}"),
        }
        pause.reply.send(AgentResume::Allow).unwrap();

        match decision_task.await.unwrap() {
            InputDecision::Replace(text) => assert_eq!(text, "rewritten"),
            other => panic!("expected Replace, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn apply_on_input_blocks_rewrite_when_approval_required_and_user_denies() {
        use crate::pause::AgentResume;

        struct Rewriter;

        #[async_trait::async_trait]
        impl HookHandler for Rewriter {
            fn name(&self) -> &str {
                "approval-rewriter"
            }
            async fn on_input(
                &self,
                _raw: &str,
                _cancel: CancellationToken,
            ) -> Result<HookOutcome> {
                Ok(HookOutcome::ReplaceInput("rewritten".into()))
            }
        }

        let (pause_tx, mut pause_rx) = crate::pause::channel(4);
        let mut reg = HookRegistry::new()
            .with_input_rewrite_pause_channel(pause_tx)
            .require_input_rewrite_approval("approval-rewriter");
        reg.register(Arc::new(Rewriter));

        let decision_task =
            tokio::spawn(async move { reg.apply_on_input("raw", CancellationToken::new()).await });
        let pause = pause_rx.recv().await.expect("approval pause should emit");
        let deny = pause
            .options
            .iter()
            .find(|option| option.key == 'd')
            .expect("deny option")
            .resume
            .clone();
        assert!(matches!(deny, AgentResume::DenyWithReason(_)));
        pause.reply.send(deny).unwrap();

        match decision_task.await.unwrap() {
            InputDecision::Block(reason) => assert!(reason.contains("user denied input rewrite")),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn apply_on_input_blocks_rewrite_when_approval_required_without_pause_channel() {
        struct Rewriter;

        #[async_trait::async_trait]
        impl HookHandler for Rewriter {
            fn name(&self) -> &str {
                "approval-rewriter"
            }
            async fn on_input(
                &self,
                _raw: &str,
                _cancel: CancellationToken,
            ) -> Result<HookOutcome> {
                Ok(HookOutcome::ReplaceInput("rewritten".into()))
            }
        }

        let mut reg = HookRegistry::new().require_input_rewrite_approval("approval-rewriter");
        reg.register(Arc::new(Rewriter));

        match reg.apply_on_input("raw", CancellationToken::new()).await {
            InputDecision::Block(reason) => assert!(reason.contains("no pause channel")),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn apply_on_input_returns_continue_when_no_hook_intervenes() {
        struct Noop;

        #[async_trait::async_trait]
        impl HookHandler for Noop {
            fn name(&self) -> &str {
                "noop"
            }
        }

        let mut reg = HookRegistry::new();
        reg.register(Arc::new(Noop));

        let decision = reg
            .apply_on_input("untouched", CancellationToken::new())
            .await;

        assert!(matches!(decision, InputDecision::Continue));
    }

    #[tokio::test]
    async fn apply_on_input_first_decision_wins_short_circuits_later_handlers() {
        struct Rewriter;
        struct LaterCounter(Arc<AtomicUsize>);

        #[async_trait::async_trait]
        impl HookHandler for Rewriter {
            fn name(&self) -> &str {
                "rewriter"
            }
            fn priority(&self) -> i32 {
                10
            }
            async fn on_input(
                &self,
                _raw: &str,
                _cancel: CancellationToken,
            ) -> Result<HookOutcome> {
                Ok(HookOutcome::ReplaceInput("rewritten".into()))
            }
        }

        #[async_trait::async_trait]
        impl HookHandler for LaterCounter {
            fn name(&self) -> &str {
                "later"
            }
            fn priority(&self) -> i32 {
                20
            }
            async fn on_input(
                &self,
                _raw: &str,
                _cancel: CancellationToken,
            ) -> Result<HookOutcome> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(HookOutcome::Continue)
            }
        }

        let later = Arc::new(AtomicUsize::new(0));
        let mut reg = HookRegistry::new();
        reg.register(Arc::new(Rewriter));
        reg.register(Arc::new(LaterCounter(later.clone())));

        let _ = reg.apply_on_input("hello", CancellationToken::new()).await;

        assert_eq!(later.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn apply_on_input_returns_block_when_hook_returns_block_input() {
        struct BlockingInput;

        #[async_trait::async_trait]
        impl HookHandler for BlockingInput {
            fn name(&self) -> &str {
                "blocking-input"
            }
            async fn on_input(
                &self,
                _raw: &str,
                _cancel: CancellationToken,
            ) -> Result<HookOutcome> {
                Ok(HookOutcome::BlockInput {
                    reason: "policy".into(),
                })
            }
        }

        let mut reg = HookRegistry::new();
        reg.register(Arc::new(BlockingInput));

        let decision = reg.apply_on_input("hello", CancellationToken::new()).await;

        match decision {
            InputDecision::Block(reason) => assert_eq!(reason, "policy"),
            other => panic!("expected Block, got {other:?}"),
        }
    }
}
