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

/// Visual classification for a pause option's pill (YYC-59). Drives color
/// — primary actions render filled, destructive in red, neutral in ink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionKind {
    Primary,
    Neutral,
    Destructive,
}

/// One inline action pill on a pause prompt (YYC-59). The hook emitting
/// the pause declares the keys/labels/colors; the TUI renders the pills
/// and routes a matching keystroke back as an `AgentResume`.
#[derive(Debug, Clone)]
pub struct PauseOption {
    /// Single-char keyboard shortcut (e.g. 'y', 'n', 'e').
    pub key: char,
    /// Human-readable label rendered inside the pill ("proceed", "deny").
    pub label: String,
    /// Color/styling class for the pill.
    pub kind: OptionKind,
    /// Resume variant to send back on press. Carrying it on the option
    /// lets each pause define its own key→action mapping without the TUI
    /// hardcoding semantics.
    pub resume: AgentResume,
}

/// One pause emission: what's being asked, plus a reply channel.
#[derive(Debug)]
pub struct AgentPause {
    pub kind: PauseKind,
    pub reply: oneshot::Sender<AgentResume>,
    /// Inline action pills the TUI should render with the prompt
    /// (YYC-59). Empty → TUI falls back to the legacy a/r/d modal.
    pub options: Vec<PauseOption>,
}

/// One occurrence of an `edit_file` substitution rendered in the diff
/// scrubber (YYC-75). The scrubber lets the user accept or reject each
/// hunk individually before any bytes hit disk.
#[derive(Debug, Clone)]
pub struct DiffScrubHunk {
    /// Byte offset in the original file where this occurrence starts.
    /// Carried so the tool can apply only the accepted hunks back to
    /// the file in stable order.
    pub offset: usize,
    /// 1-indexed source line where the hunk begins (rendering hint).
    pub line_no: usize,
    /// `old_string` content as it appears at this site (one slice per
    /// line, no trailing newline).
    pub before_lines: Vec<String>,
    /// Replacement content (one slice per line).
    pub after_lines: Vec<String>,
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
    /// Agent-initiated multiple-choice prompt (YYC-81). Each option's
    /// chosen value comes back as `AgentResume::Custom(value)`.
    UserChoice { question: String },
    /// Per-hunk accept/reject scrubber for `edit_file` (YYC-75). The TUI
    /// renders an overlay with j/k navigation and y/n toggles; on
    /// Enter the accepted indices come back as `AcceptHunks`.
    DiffScrub {
        path: String,
        hunks: Vec<DiffScrubHunk>,
    },
    /// GH issue #557: an extension's `on_input` hook proposed a
    /// `ReplaceInput` rewrite and its manifest declared
    /// `requires_user_approval = true`. The TUI shows the
    /// before/after pair; user accepts (`Allow`), denies (`Deny`),
    /// or denies with reason (`DenyWithReason`).
    InputRewriteApproval {
        extension_id: String,
        before: String,
        after: String,
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
    /// Custom value carried back to an agent-initiated pause (YYC-81
    /// `ask_user`). Hooks that receive this will typically pass the
    /// inner string through to the tool result as-is.
    Custom(String),
    /// Indices of accepted hunks in a `DiffScrub` pause (YYC-75). An
    /// empty vec means "reject all" — the tool short-circuits with no
    /// write.
    AcceptHunks(Vec<usize>),
}

pub type PauseSender = tokio::sync::mpsc::Sender<AgentPause>;
pub type PauseReceiver = tokio::sync::mpsc::Receiver<AgentPause>;

/// Convenience constructor for callers wiring up the channel.
pub fn channel(buffer: usize) -> (PauseSender, PauseReceiver) {
    tokio::sync::mpsc::channel(buffer)
}
