//! Generic agent-paused-awaiting-user-input mechanism.
//!
//! Several features need to pause the agent loop mid-flight and wait for a
//! user decision before continuing — safety-gate approval, granular tool-arg
//! confirmation, skill-save prompts, ambiguity resolution. Rather than each
//! feature inventing its own channel + UI surface, all such interruptions
//! flow through one type: an `AgentPause` is emitted, the consumer (TUI)
//! renders an appropriate prompt, and a response is delivered back via the
//! pause's `oneshot` reply channel.
//!
//! When the agent has no pause emitter wired up (CLI one-shot mode, or any
//! caller that doesn't want interactive prompts), hooks fall back to their
//! pre-pause behavior — typically: reject conservatively. See
//! [`SafetyHook`](crate::hooks::safety::SafetyHook) for the canonical example.

use serde_json::Value;
use tokio::sync::oneshot;

/// One pause emission: what's being asked, plus a reply channel.
#[derive(Debug)]
pub struct AgentPause {
    pub kind: PauseKind,
    pub reply: oneshot::Sender<AgentResume>,
}

/// What the agent is asking the user.
#[derive(Debug)]
pub enum PauseKind {
    /// A `SafetyHook` matched a dangerous shell command and wants the user
    /// to choose: allow once, allow + remember for the session, or deny.
    SafetyApproval {
        tool: String,
        command: String,
        reason: String,
    },
    /// A tool is about to run with these args; confirm before dispatching.
    /// (Replacement for the binary `yolo_mode` flag at granular scope.)
    ToolArgConfirm {
        tool: String,
        args: Value,
        summary: String,
    },
    /// "I noticed you used N+ tools to do X, save this as a skill?"
    SkillSave {
        suggested_name: String,
        body: String,
    },
}

/// The user's decision.
#[derive(Debug, Clone)]
pub enum AgentResume {
    /// Allow this single instance.
    Allow,
    /// Allow this instance and remember for the rest of the session. Hooks
    /// decide what "remember" means (e.g. `SafetyHook` adds the command to
    /// its approval cache).
    AllowAndRemember,
    /// Deny — hook returns its default `Block` outcome.
    Deny,
    /// Deny with a reason that gets surfaced to the LLM as the block reason.
    DenyWithReason(String),
}

pub type PauseSender = tokio::sync::mpsc::Sender<AgentPause>;
pub type PauseReceiver = tokio::sync::mpsc::Receiver<AgentPause>;

/// Convenience constructor for callers wiring up the channel.
pub fn channel(buffer: usize) -> (PauseSender, PauseReceiver) {
    tokio::sync::mpsc::channel(buffer)
}
