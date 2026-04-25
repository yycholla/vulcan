//! Pi-style extension hooks.
//!
//! Five wire-in points in the agent loop emit events; registered handlers may
//! return outcomes that block, modify, or extend the in-flight operation.
//! Errors and timeouts in handlers are isolated — they never break the agent
//! loop. First non-Continue outcome wins for blocking-style events; injection
//! events accumulate across all handlers.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use tokio::time::timeout;

use crate::provider::Message;
use crate::tools::ToolResult;

pub mod audit;
pub mod skills;

/// Where injected messages land in the outgoing prompt. Only honored by
/// `before_prompt` injections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectPosition {
    /// Insert immediately after the leading run of System messages, before any
    /// User/Assistant/Tool turns. Right slot for "static context" hooks like
    /// available skills.
    AfterSystem,
    /// Append to the end of the outgoing message list. Right slot for
    /// "reminders" that should be the last thing the model sees.
    Append,
}

/// What a handler returns. Each event honors a subset; unsupported variants
/// are logged and ignored.
#[derive(Debug, Clone)]
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
    /// Force the agent to keep working; the instruction is appended as a user
    /// turn and the loop continues (BeforeAgentEnd only).
    ForceContinue { instruction: String },
}

/// Decision returned to the agent loop by `before_tool_call`.
#[derive(Debug, Clone)]
pub enum ToolCallDecision {
    Continue,
    Block(String),
    ReplaceArgs(Value),
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

    async fn before_prompt(&self, _messages: &[Message]) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn before_tool_call(&self, _tool: &str, _args: &Value) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn after_tool_call(&self, _tool: &str, _result: &ToolResult) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn before_agent_end(&self, _final_response: &str) -> Result<HookOutcome> {
        Ok(HookOutcome::Continue)
    }

    async fn session_start(&self, _session_id: &str) {}

    async fn session_end(&self, _session_id: &str, _total_turns: u32) {}
}

/// Holds the registered handlers in priority order and exposes one emit method
/// per event.
pub struct HookRegistry {
    handlers: Vec<Arc<dyn HookHandler>>,
    handler_timeout: Duration,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
            handler_timeout: Duration::from_secs(30),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.handler_timeout = timeout;
        self
    }

    pub fn register(&mut self, handler: Arc<dyn HookHandler>) {
        self.handlers.push(handler);
        self.handlers.sort_by_key(|h| h.priority());
    }

    pub fn handler_count(&self) -> usize {
        self.handlers.len()
    }

    /// Emit BeforePrompt to every handler and return the outgoing prompt with
    /// all injections applied at their requested positions. The input slice is
    /// not mutated — injections are transient and never persist into
    /// conversation history.
    pub async fn apply_before_prompt(&self, messages: &[Message]) -> Vec<Message> {
        let mut after_system: Vec<Message> = Vec::new();
        let mut appended: Vec<Message> = Vec::new();

        for h in &self.handlers {
            match self.run(h, h.before_prompt(messages)).await {
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
            return messages.to_vec();
        }

        let cap = messages.len() + after_system.len() + appended.len();
        let mut out: Vec<Message> = Vec::with_capacity(cap);
        let mut injected_after_system = false;
        for m in messages {
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

    /// Emit BeforeToolCall. First non-Continue outcome wins.
    pub async fn before_tool_call(&self, tool: &str, args: &Value) -> ToolCallDecision {
        for h in &self.handlers {
            match self.run(h, h.before_tool_call(tool, args)).await {
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
    pub async fn after_tool_call(&self, tool: &str, result: &ToolResult) -> Option<ToolResult> {
        for h in &self.handlers {
            match self.run(h, h.after_tool_call(tool, result)).await {
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
    pub async fn before_agent_end(&self, response: &str) -> Option<String> {
        for h in &self.handlers {
            match self.run(h, h.before_agent_end(response)).await {
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
            let _ = timeout(
                self.handler_timeout,
                h.session_end(session_id, total_turns),
            )
            .await;
        }
    }

    async fn run<F>(&self, h: &Arc<dyn HookHandler>, fut: F) -> Option<HookOutcome>
    where
        F: std::future::Future<Output = Result<HookOutcome>>,
    {
        match timeout(self.handler_timeout, fut).await {
            Ok(Ok(o)) => Some(o),
            Ok(Err(e)) => {
                tracing::warn!("hook {} returned error: {e}", h.name());
                None
            }
            Err(_) => {
                tracing::warn!("hook {} timed out", h.name());
                None
            }
        }
    }
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}
