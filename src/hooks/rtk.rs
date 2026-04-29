//! Built-in `BeforeToolCall` hook that transparently wraps `bash` commands
//! with `rtk` (Rust Token Killer) when the binary is available on PATH.
//!
//! RTK compresses command output by 60-90% before it reaches the LLM, saving
//! context window tokens on every shell invocation. The hook is zero-cost when
//! `rtk` is not installed — the `LazyLock` check fires once, and every
//! subsequent call is a relaxed boolean read.
//!
//! Uses `ReplaceArgs` to modify the `command` field before dispatch, so the
//! tool itself stays pure and knows nothing about RTK. This also means other
//! hooks (safety, prefer-native) see the *original* command, not the wrapped
//! one — important for correctness.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::borrow::Cow;
use std::sync::LazyLock;
use tokio_util::sync::CancellationToken;

use super::{HookHandler, HookOutcome};

/// Lazily-checked availability of the `rtk` binary on PATH. Checked once
/// at first access so every bash tool call doesn't re-spawn `which`.
static RTK_AVAILABLE: LazyLock<bool> = LazyLock::new(|| {
    std::process::Command::new("which")
        .arg("rtk")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
});

pub struct RtkHook;

impl RtkHook {
    pub fn new() -> Self {
        Self
    }
}

/// If the `rtk` binary is available, wrap `command` with `rtk summary --`
/// to compress its output before returning it to the LLM. Falls through
/// to the raw command when RTK is not installed.
fn wrap_for_rtk(command: &str) -> Cow<'_, str> {
    if *RTK_AVAILABLE {
        // rtk summary runs the command and produces a heuristic summary
        // of its output — saving 60-90% on token consumption.
        Cow::Owned(format!("rtk summary -- {}", command))
    } else {
        Cow::Borrowed(command)
    }
}

#[async_trait]
impl HookHandler for RtkHook {
    fn name(&self) -> &str {
        "rtk"
    }

    /// Run after the safety gate (priority 0) and native-tools redirect
    /// (priority 5) so dangerous/redirected commands are handled first.
    /// Priority 10: runs after most built-in hooks that might block or
    /// redirect the bash call before we wrap it.
    fn priority(&self) -> i32 {
        10
    }

    async fn before_tool_call(
        &self,
        tool: &str,
        args: &Value,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        // Only wrap bash commands.
        if tool != "bash" {
            return Ok(HookOutcome::Continue);
        }

        // Don't bother if RTK isn't installed.
        if !*RTK_AVAILABLE {
            return Ok(HookOutcome::Continue);
        }

        let Some(raw_command) = args.get("command").and_then(|v| v.as_str()) else {
            return Ok(HookOutcome::Continue);
        };

        let wrapped = wrap_for_rtk(raw_command);
        match wrapped {
            Cow::Owned(ref new_command) => {
                // Clone the original args and patch the command field.
                let mut new_args = args.clone();
                new_args["command"] = Value::String(new_command.clone());
                tracing::debug!(
                    "rtk: wrapped bash command (safety check: {:50}...)",
                    raw_command.chars().take(50).collect::<String>()
                );
                Ok(HookOutcome::ReplaceArgs(new_args))
            }
            Cow::Borrowed(_) => {
                // RTK available check passed but wrap_for_rtk returned
                // borrowed — shouldn't happen, but handle gracefully.
                Ok(HookOutcome::Continue)
            }
        }
    }
}
