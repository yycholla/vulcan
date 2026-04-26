//! Native git tools (YYC-36).
//!
//! The agent gets six dedicated git tools so it can stop composing brittle
//! `git ...` shell strings through the bash tool. The current implementation
//! is a hybrid: `gix` is used for repo discovery (so a "not a repository"
//! error is a clean Rust error, not a parsed stderr line), and the actual
//! work dispatches to the `git` binary via `tokio::process::Command`. The
//! issue calls for a full `gix` port, but commit/push/branch-modify
//! semantics through gix still require non-trivial plumbing — landing
//! subprocess-backed tools first gives the agent a working surface today
//! and unblocks downstream work; per-tool migration to native gix can land
//! incrementally without reshaping the tool API.
//!
//! Each tool runs in the agent's current working directory. They surface
//! raw stdout to the LLM (no trimming) and surface stderr only when the
//! command fails.

use crate::tools::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// Run `git <args...>` in the current working directory and return its
/// output. Returns Err only for spawn failures; non-zero exit codes are
/// surfaced as a `ToolResult::err` with the captured stderr.
async fn run_git(args: &[&str], cancel: CancellationToken) -> Result<ToolResult> {
    // Repo discovery via gix. Done up front so a non-repo cwd produces a
    // clean error instead of forcing the LLM to parse `fatal: not a git
    // repository` from git's stderr.
    if gix::discover(".").is_err() {
        return Ok(ToolResult::err(
            "Not a git repository (or any parent up to the filesystem root). \
             Run from inside a working tree, or `git init` first.",
        ));
    }

    let mut cmd = Command::new("git");
    cmd.args(args);
    cmd.kill_on_drop(true);

    let child = cmd.output();
    let output = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Ok(ToolResult::err("Cancelled")),
        out = child => out?,
    };

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if output.status.success() {
        // Surface stderr in addition to stdout when both are present
        // (e.g. `git push` writes progress to stderr). Compact format
        // keeps the LLM context lean.
        let mut combined = stdout;
        if !stderr.trim().is_empty() {
            if !combined.is_empty() {
                combined.push_str("\n--- stderr ---\n");
            }
            combined.push_str(&stderr);
        }
        Ok(ToolResult::ok(if combined.is_empty() {
            "(no output)".to_string()
        } else {
            combined
        }))
    } else {
        Ok(ToolResult::err(format!(
            "git {} failed (exit {}):\n{}",
            args.join(" "),
            output.status.code().unwrap_or(-1),
            if stderr.trim().is_empty() {
                stdout
            } else {
                stderr
            }
        )))
    }
}

// ─── git_status ─────────────────────────────────────────────────────────

pub struct GitStatusTool;

#[async_trait]
impl Tool for GitStatusTool {
    fn name(&self) -> &str {
        "git_status"
    }
    fn description(&self) -> &str {
        "Show working tree status (staged, unstaged, untracked). Compact porcelain format. Use this instead of `git status` via bash."
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    async fn call(&self, _params: Value, cancel: CancellationToken) -> Result<ToolResult> {
        run_git(&["status", "--short", "--branch"], cancel).await
    }
}

// ─── git_diff ───────────────────────────────────────────────────────────

pub struct GitDiffTool;

#[async_trait]
impl Tool for GitDiffTool {
    fn name(&self) -> &str {
        "git_diff"
    }
    fn description(&self) -> &str {
        "Show diff for staged, unstaged, or specific path changes. Defaults to unstaged worktree changes. Use this instead of `git diff` via bash."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "staged": {
                    "type": "boolean",
                    "description": "If true, show staged changes (`git diff --cached`)",
                    "default": false
                },
                "path": {
                    "type": "string",
                    "description": "Optional path filter. Limits diff to this file/dir."
                }
            }
        })
    }
    async fn call(&self, params: Value, cancel: CancellationToken) -> Result<ToolResult> {
        let staged = params
            .get("staged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let path = params.get("path").and_then(|v| v.as_str());

        let mut args: Vec<&str> = vec!["diff"];
        if staged {
            args.push("--cached");
        }
        args.push("--no-color");
        if let Some(p) = path {
            args.push("--");
            args.push(p);
        }
        run_git(&args, cancel).await
    }
}

// ─── git_commit ─────────────────────────────────────────────────────────

pub struct GitCommitTool;

#[async_trait]
impl Tool for GitCommitTool {
    fn name(&self) -> &str {
        "git_commit"
    }
    fn description(&self) -> &str {
        "Create a commit with the given message. Set `all=true` to stage all tracked changes first. Use this instead of `git commit -m` via bash."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "message": { "type": "string", "description": "Commit message" },
                "all": {
                    "type": "boolean",
                    "description": "Stage all tracked modifications before committing (`git commit -a`)",
                    "default": false
                }
            },
            "required": ["message"]
        })
    }
    async fn call(&self, params: Value, cancel: CancellationToken) -> Result<ToolResult> {
        let message = params
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("message required"))?;
        let all = params
            .get("all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut args: Vec<&str> = vec!["commit"];
        if all {
            args.push("-a");
        }
        args.push("-m");
        args.push(message);
        run_git(&args, cancel).await
    }
}

// ─── git_push ───────────────────────────────────────────────────────────

pub struct GitPushTool;

#[async_trait]
impl Tool for GitPushTool {
    fn name(&self) -> &str {
        "git_push"
    }
    fn description(&self) -> &str {
        "Push the current branch to its remote tracking branch. Set `set_upstream=true` to push and create the upstream link in one go. Use this instead of `git push` via bash."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "remote": { "type": "string", "description": "Remote name", "default": "origin" },
                "branch": { "type": "string", "description": "Branch to push (defaults to current)" },
                "set_upstream": {
                    "type": "boolean",
                    "description": "Use -u to track the remote branch on first push",
                    "default": false
                }
            }
        })
    }
    async fn call(&self, params: Value, cancel: CancellationToken) -> Result<ToolResult> {
        let remote = params
            .get("remote")
            .and_then(|v| v.as_str())
            .unwrap_or("origin");
        let branch = params.get("branch").and_then(|v| v.as_str());
        let set_upstream = params
            .get("set_upstream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut args: Vec<&str> = vec!["push"];
        if set_upstream {
            args.push("-u");
        }
        args.push(remote);
        if let Some(b) = branch {
            args.push(b);
        }
        run_git(&args, cancel).await
    }
}

// ─── git_branch ─────────────────────────────────────────────────────────

pub struct GitBranchTool;

#[async_trait]
impl Tool for GitBranchTool {
    fn name(&self) -> &str {
        "git_branch"
    }
    fn description(&self) -> &str {
        "List, create, or switch branches. Default action lists local branches with the current one starred. Use this instead of `git branch` / `git checkout` via bash."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "create", "switch"],
                    "description": "Operation to perform",
                    "default": "list"
                },
                "name": {
                    "type": "string",
                    "description": "Branch name (required for create / switch)"
                }
            }
        })
    }
    async fn call(&self, params: Value, cancel: CancellationToken) -> Result<ToolResult> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list");
        let name = params.get("name").and_then(|v| v.as_str());

        match action {
            "list" => run_git(&["branch", "--list", "-vv"], cancel).await,
            "create" => {
                let n =
                    name.ok_or_else(|| anyhow::anyhow!("name required for action='create'"))?;
                // -b creates and switches in one step; matches the
                // common agent intent "make this branch and use it".
                run_git(&["checkout", "-b", n], cancel).await
            }
            "switch" => {
                let n =
                    name.ok_or_else(|| anyhow::anyhow!("name required for action='switch'"))?;
                run_git(&["checkout", n], cancel).await
            }
            other => Ok(ToolResult::err(format!(
                "Unknown action '{other}'. Use 'list', 'create', or 'switch'."
            ))),
        }
    }
}

// ─── git_log ────────────────────────────────────────────────────────────

pub struct GitLogTool;

#[async_trait]
impl Tool for GitLogTool {
    fn name(&self) -> &str {
        "git_log"
    }
    fn description(&self) -> &str {
        "Show recent commit history (oneline format). Defaults to last 10 commits. Use this instead of `git log` via bash."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "integer",
                    "description": "Max commits to show",
                    "default": 10
                },
                "branch": {
                    "type": "string",
                    "description": "Branch or ref to log (defaults to HEAD)"
                }
            }
        })
    }
    async fn call(&self, params: Value, cancel: CancellationToken) -> Result<ToolResult> {
        let limit = params.get("limit").and_then(|v| v.as_i64()).unwrap_or(10);
        let limit = limit.clamp(1, 200).to_string();
        let branch = params.get("branch").and_then(|v| v.as_str());

        let mut args: Vec<&str> = vec!["log", "--oneline", "-n", limit.as_str()];
        if let Some(b) = branch {
            args.push(b);
        }
        run_git(&args, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    /// Run `args` in `cwd`. Wraps tokio's Command directly so the test
    /// doesn't need to hop through the public Tool trait.
    async fn git(cwd: &std::path::Path, args: &[&str]) {
        let status = tokio::process::Command::new("git")
            .current_dir(cwd)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .await
            .expect("git ran");
        assert!(status.success(), "git {:?} failed", args);
    }

    #[tokio::test]
    async fn git_status_in_a_real_repo_lists_modifications() {
        let dir = tempdir().unwrap();
        let cwd = dir.path();
        git(cwd, &["init", "-q"]).await;
        std::fs::write(cwd.join("a.txt"), "hello\n").unwrap();
        // The tool reads from the process cwd, so chdir for this test.
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(cwd).unwrap();
        let result = GitStatusTool
            .call(json!({}), CancellationToken::new())
            .await
            .unwrap();
        std::env::set_current_dir(prev).unwrap();
        assert!(!result.is_error, "got err: {}", result.output);
        assert!(
            result.output.contains("a.txt"),
            "output should mention the untracked file: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn git_status_outside_repo_returns_clean_error() {
        let dir = tempdir().unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let result = GitStatusTool
            .call(json!({}), CancellationToken::new())
            .await
            .unwrap();
        std::env::set_current_dir(prev).unwrap();
        assert!(result.is_error);
        assert!(
            result.output.contains("Not a git repository"),
            "got {}",
            result.output
        );
    }
}
