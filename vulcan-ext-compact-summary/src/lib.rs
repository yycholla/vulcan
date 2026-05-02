//! Sample compaction-control extension. It replaces built-in
//! summarization with a deterministic "system + compact summary + last
//! twenty turns" history rewrite.
//!
//! Demo modes for validation/override paths:
//! - `VULCAN_EXT_COMPACT_SUMMARY_MODE=bad` returns an invalid rewrite.
//! - `VULCAN_EXT_COMPACT_SUMMARY_MODE=block` vetoes compaction.

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use vulcan::extensions::api::{
    DaemonCodeExtension, ExtensionRegistration, SessionExtension, SessionExtensionCtx,
};
use vulcan::extensions::{
    ExtensionCapability, ExtensionMetadata, ExtensionSource, ExtensionStatus,
};
use vulcan::hooks::{HookHandler, HookOutcome};
use vulcan::provider::Message;

const ID: &str = "compact-summary";
const KEEP_TURNS: usize = 20;

pub struct CompactSummaryExtension;

impl Default for CompactSummaryExtension {
    fn default() -> Self {
        Self
    }
}

impl DaemonCodeExtension for CompactSummaryExtension {
    fn metadata(&self) -> ExtensionMetadata {
        let mut m = ExtensionMetadata::new(
            ID,
            "Compact Summary",
            env!("CARGO_PKG_VERSION"),
            ExtensionSource::Builtin,
        );
        m.status = ExtensionStatus::Active;
        m.capabilities = vec![ExtensionCapability::HookHandler];
        m.description = "Rewrites compaction history with system context, a compact summary, and the last twenty turns.".to_string();
        m
    }

    fn instantiate(&self, _ctx: SessionExtensionCtx) -> Arc<dyn SessionExtension> {
        Arc::new(CompactSummarySession)
    }
}

struct CompactSummarySession;

impl SessionExtension for CompactSummarySession {
    fn hook_handlers(&self) -> Vec<Arc<dyn HookHandler>> {
        vec![Arc::new(CompactSummaryHook {
            mode: CompactSummaryMode::from_env(),
        })]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompactSummaryMode {
    Rewrite,
    BadRewrite,
    Block,
}

impl CompactSummaryMode {
    fn from_env() -> Self {
        match std::env::var("VULCAN_EXT_COMPACT_SUMMARY_MODE")
            .unwrap_or_default()
            .as_str()
        {
            "bad" => Self::BadRewrite,
            "block" => Self::Block,
            _ => Self::Rewrite,
        }
    }
}

struct CompactSummaryHook {
    mode: CompactSummaryMode,
}

#[async_trait]
impl HookHandler for CompactSummaryHook {
    fn name(&self) -> &str {
        ID
    }

    async fn on_session_before_compact(
        &self,
        messages: &[Message],
        _cancel: CancellationToken,
    ) -> anyhow::Result<HookOutcome> {
        match self.mode {
            CompactSummaryMode::Rewrite => Ok(HookOutcome::RewriteHistory(rewrite(messages))),
            CompactSummaryMode::BadRewrite => {
                Ok(HookOutcome::RewriteHistory(vec![Message::User {
                    content: "invalid rewrite without a system message".into(),
                }]))
            }
            CompactSummaryMode::Block => Ok(HookOutcome::Block {
                reason: "compact-summary demo block".into(),
            }),
        }
    }
}

fn rewrite(messages: &[Message]) -> Vec<Message> {
    let system_messages: Vec<Message> = messages
        .iter()
        .filter(|m| matches!(m, Message::System { .. }))
        .cloned()
        .collect();
    let keep_start = last_turn_window_start(messages, KEEP_TURNS);
    let omitted = messages[..keep_start].len();

    let mut out = if system_messages.is_empty() {
        vec![Message::System {
            content: "Vulcan session".into(),
        }]
    } else {
        system_messages
    };
    out.push(Message::System {
        content: format!(
            "Extension compact summary:\n- omitted {omitted} older messages\n- kept last {KEEP_TURNS} turns when available\n- tool registry summary: preserve tool call/result pairs in the kept window"
        ),
    });
    out.extend(
        messages[keep_start..]
            .iter()
            .filter(|m| !matches!(m, Message::System { .. }))
            .cloned(),
    );
    out
}

fn last_turn_window_start(messages: &[Message], turns: usize) -> usize {
    let mut seen_users = 0usize;
    for (idx, msg) in messages.iter().enumerate().rev() {
        if matches!(msg, Message::User { .. }) {
            seen_users += 1;
            if seen_users == turns {
                return idx;
            }
        }
    }
    messages
        .iter()
        .position(|m| !matches!(m, Message::System { .. }))
        .unwrap_or(messages.len())
}

inventory::submit! {
    ExtensionRegistration {
        register: || Arc::new(CompactSummaryExtension) as Arc<dyn DaemonCodeExtension>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vulcan::hooks::{RewriteRejection, validate_rewrite_history};

    fn ctx() -> SessionExtensionCtx {
        SessionExtensionCtx {
            cwd: std::path::PathBuf::from("/tmp/test"),
            session_id: "test-session".to_string(),
        }
    }

    fn long_history() -> Vec<Message> {
        let mut messages = vec![Message::System {
            content: "system".into(),
        }];
        for i in 0..30 {
            messages.push(Message::User {
                content: format!("user {i}"),
            });
            messages.push(Message::Assistant {
                content: Some(format!("assistant {i}")),
                tool_calls: None,
                reasoning_content: None,
            });
        }
        messages
    }

    #[tokio::test]
    async fn rewrite_mode_returns_valid_shorter_history() {
        let session = CompactSummaryExtension.instantiate(ctx());
        let handlers = session.hook_handlers();
        let input = long_history();

        let outcome = handlers[0]
            .on_session_before_compact(&input, CancellationToken::new())
            .await
            .expect("hook ok");

        match outcome {
            HookOutcome::RewriteHistory(messages) => {
                assert!(validate_rewrite_history(&input, &messages).is_ok());
                assert!(messages.iter().any(|m| matches!(
                    m,
                    Message::System { content } if content.contains("Extension compact summary")
                )));
            }
            other => panic!("expected RewriteHistory, got {other:?}"),
        }
    }

    #[test]
    fn bad_mode_exercises_validator_rejection() {
        let input = long_history();
        let bad = vec![Message::User {
            content: "bad".into(),
        }];

        assert!(matches!(
            validate_rewrite_history(&input, &bad),
            Err(RewriteRejection::MissingSystem)
        ));
    }

    #[tokio::test]
    async fn block_mode_vetoes_compaction_for_override_demo() {
        let hook = CompactSummaryHook {
            mode: CompactSummaryMode::Block,
        };

        let outcome = hook
            .on_session_before_compact(&long_history(), CancellationToken::new())
            .await
            .expect("hook ok");

        assert!(matches!(outcome, HookOutcome::Block { reason } if reason.contains("demo block")));
    }
}
