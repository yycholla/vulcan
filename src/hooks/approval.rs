//! Per-tool approval hook (YYC-76).
//!
//! Gates BeforeToolCall by the configured approval mode for each tool.
//! Modes:
//! - `Always`: run without prompting (default — back-compat with the
//!   pre-YYC-76 zero-config experience).
//! - `Session`: pause on first call; cache approval; run silently
//!   thereafter for the same session.
//! - `Ask`: pause every call.
//!
//! Pauses go through the existing AgentPause channel so the TUI's
//! YYC-59 inline pills handle the user choice. When no pause emitter
//! is wired (CLI one-shot mode), the hook conservatively blocks for
//! `Ask` / `Session` modes so the agent doesn't silently bypass a
//! requested gate.

use crate::config::{ApprovalConfig, ApprovalMode};
use crate::hooks::{HookHandler, HookOutcome};
use crate::pause::{
    AgentPause, AgentResume, OptionKind, PauseKind, PauseOption, PauseSender,
};
use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Mutex;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub struct ApprovalHook {
    cfg: ApprovalConfig,
    pause_tx: Option<PauseSender>,
    /// Per-tool session approvals — one entry per `(tool_name)` after
    /// the user picks "approve session".
    session_allow: Mutex<HashSet<String>>,
}

impl ApprovalHook {
    pub fn new(cfg: ApprovalConfig, pause_tx: Option<PauseSender>) -> Self {
        Self {
            cfg,
            pause_tx,
            session_allow: Mutex::new(HashSet::new()),
        }
    }

    pub fn auto_deny(cfg: ApprovalConfig) -> Self {
        Self::new(cfg, None)
    }
}

#[async_trait::async_trait]
impl HookHandler for ApprovalHook {
    fn name(&self) -> &str {
        "approval_gate"
    }

    fn priority(&self) -> i32 {
        // Run early — well before SafetyHook (50) — so explicit
        // session-allows for a tool can short-circuit the
        // command-pattern check.
        20
    }

    async fn before_tool_call(
        &self,
        tool: &str,
        args: &Value,
        cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        let mode = self.cfg.mode_for(tool);
        if matches!(mode, ApprovalMode::Always) {
            return Ok(HookOutcome::Continue);
        }
        if matches!(mode, ApprovalMode::Session)
            && self.session_allow.lock().unwrap().contains(tool)
        {
            return Ok(HookOutcome::Continue);
        }

        // Need user input but no channel wired — fail safe (block).
        let tx = match &self.pause_tx {
            Some(t) => t,
            None => {
                return Ok(HookOutcome::Block {
                    reason: format!(
                        "tool '{tool}' requires {mode:?} approval but no pause channel is wired"
                    ),
                });
            }
        };

        let summary = format!(
            "Tool '{tool}' requires approval (mode: {mode:?}). Args: {args}"
        );
        let options = match mode {
            ApprovalMode::Session => vec![
                PauseOption {
                    key: 'y',
                    label: "approve once".into(),
                    kind: OptionKind::Primary,
                    resume: AgentResume::Allow,
                },
                PauseOption {
                    key: 's',
                    label: "approve session".into(),
                    kind: OptionKind::Neutral,
                    resume: AgentResume::AllowAndRemember,
                },
                PauseOption {
                    key: 'n',
                    label: "deny".into(),
                    kind: OptionKind::Destructive,
                    resume: AgentResume::Deny,
                },
            ],
            ApprovalMode::Ask => vec![
                PauseOption {
                    key: 'y',
                    label: "approve".into(),
                    kind: OptionKind::Primary,
                    resume: AgentResume::Allow,
                },
                PauseOption {
                    key: 'n',
                    label: "deny".into(),
                    kind: OptionKind::Destructive,
                    resume: AgentResume::Deny,
                },
            ],
            ApprovalMode::Always => unreachable!(),
        };

        let (reply_tx, reply_rx) = oneshot::channel();
        let pause = AgentPause {
            kind: PauseKind::ToolArgConfirm {
                tool: tool.to_string(),
                args: args.clone(),
                summary,
            },
            reply: reply_tx,
            options,
        };

        if tx.send(pause).await.is_err() {
            return Ok(HookOutcome::Block {
                reason: "approval channel dropped".into(),
            });
        }

        let resume = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Ok(HookOutcome::Block {
                    reason: "Cancelled while awaiting approval".into(),
                });
            }
            r = reply_rx => r,
        };

        match resume {
            Ok(AgentResume::Allow) => Ok(HookOutcome::Continue),
            Ok(AgentResume::AllowAndRemember) => {
                self.session_allow.lock().unwrap().insert(tool.to_string());
                Ok(HookOutcome::Continue)
            }
            Ok(AgentResume::Deny) => Ok(HookOutcome::Block {
                reason: "user denied".into(),
            }),
            Ok(AgentResume::DenyWithReason(r)) => Ok(HookOutcome::Block { reason: r }),
            Ok(AgentResume::Custom(_)) => Ok(HookOutcome::Block {
                reason: "approval prompt got a custom response — denying".into(),
            }),
            Err(_) => Ok(HookOutcome::Block {
                reason: "approval channel closed".into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::HookOutcome;

    #[tokio::test]
    async fn approval_hook_auto_denies_when_no_pause_channel() {
        let cfg = ApprovalConfig {
            default: ApprovalMode::Ask,
            per_tool: Default::default(),
        };
        let hook = ApprovalHook::auto_deny(cfg);

        let decision = hook
            .before_tool_call(
                "write_file",
                &serde_json::json!({"path": "/tmp/x", "content": "hi"}),
                CancellationToken::new(),
            )
            .await
            .expect("hook result");

        match decision {
            HookOutcome::Block { reason } => {
                assert!(reason.contains("requires Ask approval"));
                assert!(reason.contains("no pause channel"));
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn approval_hook_auto_deny_still_allows_always_mode() {
        let hook = ApprovalHook::auto_deny(ApprovalConfig::default());

        let decision = hook
            .before_tool_call("read_file", &serde_json::json!({}), CancellationToken::new())
            .await
            .expect("hook result");

        assert!(matches!(decision, HookOutcome::Continue));
    }
}
