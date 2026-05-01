//! Sample input-intercept extension. Expands `!!` to the most recent
//! non-`!!` user input the extension has observed for the **Session**.
//!
//! Daemon-side cargo-crate extension under GH issue #557. Self-
//! registers via `inventory::submit!`. Manifest declares
//! `capabilities = ["input_interceptor"]` and
//! `requires_user_approval = false` so rewrites land without a
//! pause prompt; flip the manifest flag to exercise the
//! `AgentPause::InputRewriteApproval` path.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use vulcan::extensions::api::{
    DaemonCodeExtension, ExtensionRegistration, SessionExtension, SessionExtensionCtx,
};
use vulcan::extensions::{
    ExtensionCapability, ExtensionMetadata, ExtensionSource, ExtensionStatus,
};
use vulcan::hooks::{HookHandler, HookOutcome};

const ID: &str = "input-demo";

pub struct InputDemoExtension;

impl Default for InputDemoExtension {
    fn default() -> Self {
        Self
    }
}

impl DaemonCodeExtension for InputDemoExtension {
    fn metadata(&self) -> ExtensionMetadata {
        let mut m = ExtensionMetadata::new(
            ID,
            "Input Demo",
            env!("CARGO_PKG_VERSION"),
            ExtensionSource::Builtin,
        );
        m.status = ExtensionStatus::Active;
        m.capabilities = vec![ExtensionCapability::InputInterceptor];
        m.description = "Expands `!!` to the previous non-`!!` user message.".to_string();
        m
    }

    fn instantiate(&self, _ctx: SessionExtensionCtx) -> Arc<dyn SessionExtension> {
        Arc::new(InputDemoSession {
            last_input: Arc::new(Mutex::new(None)),
        })
    }
}

struct InputDemoSession {
    last_input: Arc<Mutex<Option<String>>>,
}

impl SessionExtension for InputDemoSession {
    fn hook_handlers(&self) -> Vec<Arc<dyn HookHandler>> {
        vec![Arc::new(InputDemoHook {
            last_input: self.last_input.clone(),
        })]
    }
}

struct InputDemoHook {
    last_input: Arc<Mutex<Option<String>>>,
}

#[async_trait]
impl HookHandler for InputDemoHook {
    fn name(&self) -> &str {
        ID
    }

    async fn on_input(&self, raw: &str, _cancel: CancellationToken) -> anyhow::Result<HookOutcome> {
        let trimmed = raw.trim();
        if trimmed == "!!" {
            // Replace with the previous user input when one exists;
            // otherwise pass the literal `!!` through unchanged so the
            // user sees no surprise rewrite on the very first turn.
            return match self.last_input.lock().clone() {
                Some(prev) => Ok(HookOutcome::ReplaceInput(prev)),
                None => Ok(HookOutcome::Continue),
            };
        }
        // Cache the latest non-`!!` input for the next turn.
        *self.last_input.lock() = Some(raw.to_string());
        Ok(HookOutcome::Continue)
    }
}

inventory::submit! {
    ExtensionRegistration {
        register: || Arc::new(InputDemoExtension) as Arc<dyn DaemonCodeExtension>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> SessionExtensionCtx {
        SessionExtensionCtx {
            cwd: std::path::PathBuf::from("/tmp/test"),
            session_id: "test-session".to_string(),
        }
    }

    #[tokio::test]
    async fn first_turn_passes_bang_bang_through_unchanged() {
        let session = InputDemoExtension.instantiate(ctx());
        let handlers = session.hook_handlers();
        let outcome = handlers[0]
            .on_input("!!", CancellationToken::new())
            .await
            .expect("on_input ok");
        assert!(matches!(outcome, HookOutcome::Continue));
    }

    #[tokio::test]
    async fn bang_bang_after_real_input_replaces_with_previous_message() {
        let session = InputDemoExtension.instantiate(ctx());
        let handlers = session.hook_handlers();

        let _ = handlers[0]
            .on_input("show me the code", CancellationToken::new())
            .await;

        let outcome = handlers[0]
            .on_input("!!", CancellationToken::new())
            .await
            .expect("on_input ok");
        match outcome {
            HookOutcome::ReplaceInput(text) => assert_eq!(text, "show me the code"),
            other => panic!("expected ReplaceInput, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn non_bang_bang_input_is_cached_and_passes_through() {
        let session = InputDemoExtension.instantiate(ctx());
        let handlers = session.hook_handlers();
        let outcome = handlers[0]
            .on_input("hello", CancellationToken::new())
            .await
            .expect("on_input ok");
        assert!(matches!(outcome, HookOutcome::Continue));
    }
}
