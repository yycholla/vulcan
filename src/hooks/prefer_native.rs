//! Built-in `BeforeToolCall` hook that nudges the agent off `bash` when
//! a native tool covers the same job (YYC-87 under the YYC-84 epic).
//!
//! Mode is driven by `[tools].native_enforcement` (YYC-89):
//!
//! * `Off`   — hook isn't registered (caller decides).
//! * `Warn`  — log + audit, but pass through.
//! * `Block` — return `HookOutcome::Block` with a redirect message so
//!             the model retries with the native tool.
//!
//! The detector deliberately stays dumb: first-word match against the
//! command string, with a quick "this is doing pipes / multi-stage
//! shell" escape hatch. False positives produce a recoverable block;
//! false negatives just leave bash usage unchanged. We optimise for
//! the boring cases the agent invokes ten times a session — single-
//! command `git status`, `cargo check`, `rg foo`, `cat path`.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::config::NativeEnforcement;

use super::{HookHandler, HookOutcome};

pub struct PreferNativeToolsHook {
    mode: NativeEnforcement,
}

impl PreferNativeToolsHook {
    pub fn new(mode: NativeEnforcement) -> Self {
        Self { mode }
    }
}

#[async_trait]
impl HookHandler for PreferNativeToolsHook {
    fn name(&self) -> &str {
        "prefer-native-tools"
    }

    fn priority(&self) -> i32 {
        // Run after the safety gate (priority 0) so blocked-dangerous
        // commands still get the safety reason rather than a redirect.
        // Run before audit (priority 1) so the audit log records the
        // redirect, not the would-have-been bash call.
        5
    }

    async fn before_tool_call(
        &self,
        tool: &str,
        args: &Value,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        if matches!(self.mode, NativeEnforcement::Off) {
            return Ok(HookOutcome::Continue);
        }
        if tool != "bash" {
            return Ok(HookOutcome::Continue);
        }
        let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
            return Ok(HookOutcome::Continue);
        };
        let Some(redirect) = match_native_redirect(command) else {
            return Ok(HookOutcome::Continue);
        };

        match self.mode {
            NativeEnforcement::Off => Ok(HookOutcome::Continue),
            NativeEnforcement::Warn => {
                tracing::info!("prefer-native-tools (warn): bash `{command}` could use {redirect}");
                Ok(HookOutcome::Continue)
            }
            NativeEnforcement::Block => Ok(HookOutcome::Block {
                reason: format!(
                    "{redirect} (blocked by native_enforcement=block; bash command was: `{command}`)"
                ),
            }),
        }
    }
}

/// Stable short tag the audit telemetry (YYC-88) groups counts by.
/// Matches the bash-tool head — `rg`, `grep`, `cat`, `git`, etc.
pub fn match_native_category(command: &str) -> Option<&'static str> {
    let cmd = command.trim();
    if cmd.is_empty() {
        return None;
    }
    if cmd.contains('|')
        || cmd.contains("&&")
        || cmd.contains(';')
        || cmd.contains('>')
        || cmd.contains("$(")
    {
        return None;
    }
    let mut parts = cmd.split_ascii_whitespace();
    let head = parts.next()?;
    let sub = parts.next();
    match head {
        "rg" => Some("rg"),
        "grep" => Some("grep"),
        "find" => Some("find"),
        "ls" => Some("ls"),
        "cat" => Some("cat"),
        "head" => Some("head"),
        "tail" => Some("tail"),
        "cargo" => match sub {
            Some("check") | Some("build") => Some("cargo"),
            _ => None,
        },
        "git" => match sub {
            Some("status") | Some("diff") | Some("log") | Some("commit") | Some("push")
            | Some("branch") | Some("checkout") => Some("git"),
            _ => None,
        },
        _ => None,
    }
}

/// Inspect a bash `command` string and return a one-line redirect
/// message naming the native tool the agent should use, or `None` when
/// the command should pass through. Trims leading whitespace and bails
/// out as soon as the command looks like multi-stage shell — pipes,
/// `&&`, `;`, redirection, or `$( ... )` — because the heuristic can't
/// honestly translate those.
pub fn match_native_redirect(command: &str) -> Option<&'static str> {
    let cmd = command.trim();
    if cmd.is_empty() {
        return None;
    }

    // Bail on anything that looks like pipes / multi-stage shell.
    // Native tools can't carry over xargs, awk filters, redirects.
    if cmd.contains('|')
        || cmd.contains("&&")
        || cmd.contains(';')
        || cmd.contains('>')
        || cmd.contains("$(")
    {
        return None;
    }

    // Split on whitespace and inspect the first token (and optionally
    // the second for `cargo check`, `git status`, etc.).
    let mut parts = cmd.split_ascii_whitespace();
    let head = parts.next()?;
    let sub = parts.next();

    match head {
        "rg" => Some("Use the `search_files` tool — gitignore-aware regex search."),
        "grep" => {
            // `grep -r/-R ...` or `grep -rn ...` — recursive forms map
            // to search_files. Plain `grep pattern file` could be a
            // single-file scan; still nudge to search_files since it
            // handles both shapes.
            Some("Use the `search_files` tool — gitignore-aware regex search.")
        }
        "find" => Some("Use the `list_files` tool — gitignore-aware tree listing."),
        "ls" => Some("Use the `list_files` tool — structured directory listing."),
        "cat" | "head" | "tail" => Some("Use the `read_file` tool — line-numbered file read."),
        "cargo" => match sub {
            Some("check") | Some("build") => Some(
                "Use the `cargo_check` tool — structured compiler diagnostics with file/line info.",
            ),
            _ => None,
        },
        "git" => match sub {
            Some("status") => Some("Use the `git_status` tool."),
            Some("diff") => Some("Use the `git_diff` tool."),
            Some("log") => Some("Use the `git_log` tool."),
            Some("commit") => Some("Use the `git_commit` tool."),
            Some("push") => Some("Use the `git_push` tool."),
            Some("branch") | Some("checkout") => Some("Use the `git_branch` tool."),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn match_redirect_catches_ripgrep_and_grep_recursive() {
        assert!(
            match_native_redirect("rg foo")
                .unwrap()
                .contains("search_files")
        );
        assert!(
            match_native_redirect("rg -i pattern src/")
                .unwrap()
                .contains("search_files")
        );
        assert!(
            match_native_redirect("grep -r needle ./")
                .unwrap()
                .contains("search_files")
        );
        assert!(
            match_native_redirect("grep -rn x")
                .unwrap()
                .contains("search_files")
        );
    }

    #[test]
    fn match_redirect_catches_cat_head_tail_ls_find() {
        assert!(
            match_native_redirect("cat README.md")
                .unwrap()
                .contains("read_file")
        );
        assert!(
            match_native_redirect("head -20 main.rs")
                .unwrap()
                .contains("read_file")
        );
        assert!(
            match_native_redirect("tail Cargo.toml")
                .unwrap()
                .contains("read_file")
        );
        assert!(
            match_native_redirect("ls -la")
                .unwrap()
                .contains("list_files")
        );
        assert!(
            match_native_redirect("find . -name '*.rs'")
                .unwrap()
                .contains("list_files")
        );
    }

    #[test]
    fn match_redirect_catches_cargo_check_and_git_subcommands() {
        assert!(
            match_native_redirect("cargo check")
                .unwrap()
                .contains("cargo_check")
        );
        assert!(
            match_native_redirect("cargo build")
                .unwrap()
                .contains("cargo_check")
        );
        assert!(
            match_native_redirect("git status")
                .unwrap()
                .contains("git_status")
        );
        assert!(
            match_native_redirect("git diff --cached")
                .unwrap()
                .contains("git_diff")
        );
        assert!(
            match_native_redirect("git log --oneline")
                .unwrap()
                .contains("git_log")
        );
        assert!(
            match_native_redirect("git commit -m fixup")
                .unwrap()
                .contains("git_commit")
        );
    }

    #[test]
    fn match_redirect_passes_through_pipes_and_multi_stage() {
        assert_eq!(match_native_redirect("rg foo | head -3"), None);
        assert_eq!(match_native_redirect("ls && cd"), None);
        assert_eq!(match_native_redirect("git status; echo ok"), None);
        assert_eq!(match_native_redirect("cat foo > out"), None);
        assert_eq!(match_native_redirect("ls $(pwd)"), None);
    }

    #[test]
    fn match_redirect_passes_through_unrelated_commands() {
        assert_eq!(match_native_redirect("npm install"), None);
        assert_eq!(match_native_redirect("python script.py"), None);
        assert_eq!(match_native_redirect("cargo test"), None);
        assert_eq!(match_native_redirect("git rebase main"), None);
        assert_eq!(match_native_redirect(""), None);
        assert_eq!(match_native_redirect("   "), None);
    }

    #[tokio::test]
    async fn hook_block_mode_blocks_bash_redirect() {
        let hook = PreferNativeToolsHook::new(NativeEnforcement::Block);
        let args = json!({ "command": "rg foo" });
        let outcome = hook
            .before_tool_call("bash", &args, CancellationToken::new())
            .await
            .unwrap();
        match outcome {
            HookOutcome::Block { reason } => {
                assert!(reason.contains("search_files"), "reason was {reason:?}");
                assert!(reason.contains("rg foo"), "reason was {reason:?}");
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hook_warn_mode_passes_through() {
        let hook = PreferNativeToolsHook::new(NativeEnforcement::Warn);
        let args = json!({ "command": "rg foo" });
        let outcome = hook
            .before_tool_call("bash", &args, CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(outcome, HookOutcome::Continue));
    }

    #[tokio::test]
    async fn hook_off_mode_short_circuits() {
        let hook = PreferNativeToolsHook::new(NativeEnforcement::Off);
        let args = json!({ "command": "rg foo" });
        let outcome = hook
            .before_tool_call("bash", &args, CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(outcome, HookOutcome::Continue));
    }

    #[tokio::test]
    async fn hook_ignores_non_bash_tools() {
        let hook = PreferNativeToolsHook::new(NativeEnforcement::Block);
        let args = json!({ "command": "rg foo" });
        let outcome = hook
            .before_tool_call("write_file", &args, CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(outcome, HookOutcome::Continue));
    }
}
