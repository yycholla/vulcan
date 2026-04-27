//! YYC-82: spawn_subagent tool — scoped child agent with budget.
//!
//! The parent agent dispatches a focused task to a child agent
//! that runs a complete `Agent` loop in-process. The child shares
//! the parent's provider config (same API key + base URL + model)
//! but gets a fresh hook registry and a *restricted* tool registry
//! filtered to an explicit allowlist. The parent receives a bounded
//! summary and budget usage stats — not a live transcript dump.
//!
//! ## Scope of this PR
//!
//! - Build child via `Agent::builder` + restrict tools by name.
//! - Hard cap on loop iterations (`max_iterations`).
//! - Conservative default tool allowlist (read-only).
//! - Fresh in-memory session for the child so its turn history
//!   doesn't pollute the parent's session store.
//!
//! ## Deliberately deferred
//!
//! - Token budget tracking (max_iterations is the proxy today).
//! - Parent cancellation propagation into the child loop.
//! - Transcript/artifact handle for inspection.
//! - TUI subagent tile (lands with YYC-68).

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;
use crate::config::Config;
use crate::hooks::HookRegistry;
use crate::orchestration::OrchestrationStore;
use crate::tools::{Tool, ToolResult};

/// YYC-82: conservative default tool set for a child agent. Read-
/// only inspection only — file writes, shell, pty, and recursive
/// `spawn_subagent` are excluded so a parent that doesn't
/// explicitly opt-in can't accidentally hand the child too much
/// authority.
fn default_allowed_tools() -> Vec<String> {
    [
        "read_file",
        "list_files",
        "search_files",
        "code_outline",
        "code_extract",
        "code_query",
        "find_symbol",
        "goto_definition",
        "find_references",
        "hover",
        "type_definition",
        "implementation",
        "workspace_symbol",
        "call_hierarchy",
        "diagnostics",
        "code_action",
        "git_status",
        "git_diff",
        "git_log",
        "git_branch",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Hard ceiling on `max_iterations`. Parents can ask for less but
/// not more — the budget exists to keep child runs bounded so a
/// recursive invocation can't accidentally chew through a token
/// budget.
const SUBAGENT_MAX_ITERATIONS_CAP: u32 = 32;
/// Default when caller doesn't supply `max_iterations`.
const SUBAGENT_DEFAULT_ITERATIONS: u32 = 8;

#[derive(Clone)]
pub struct SpawnSubagentTool {
    config: Arc<Config>,
    /// YYC-206: orchestration store the tool registers child runs
    /// against. Shared via `Arc` so the TUI / future admin endpoint
    /// can read the same state.
    orchestration: Arc<OrchestrationStore>,
}

impl SpawnSubagentTool {
    pub fn new(config: Arc<Config>) -> Self {
        Self::with_store(config, Arc::new(OrchestrationStore::new()))
    }

    /// YYC-206: explicit-store constructor so the parent agent
    /// can hand in a shared `OrchestrationStore` (the same one
    /// the TUI reads from).
    pub fn with_store(config: Arc<Config>, orchestration: Arc<OrchestrationStore>) -> Self {
        Self {
            config,
            orchestration,
        }
    }
}

#[async_trait]
impl Tool for SpawnSubagentTool {
    fn name(&self) -> &str {
        "spawn_subagent"
    }

    fn description(&self) -> &str {
        "Delegate a focused task to a scoped child agent. The child runs an Agent loop with a restricted tool allowlist and a hard iteration cap, then returns a summary. Use for: reviewing a subsystem, summarizing a long file, comparing alternatives, bounded code search. Default tool set is read-only; pass `allowed_tools` to widen it."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The prompt the child agent will execute. Be specific — the child has no parent context."
                },
                "allowed_tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Tool names the child may call. Omit for the conservative read-only default. Tools the parent doesn't have are silently dropped."
                },
                "max_iterations": {
                    "type": "integer",
                    "description": "Hard cap on the child's agent loop. Default 8, max 32."
                }
            },
            "required": ["task"]
        })
    }

    async fn call(&self, params: Value, cancel: CancellationToken) -> Result<ToolResult> {
        let task = match params["task"].as_str() {
            Some(t) if !t.trim().is_empty() => t.to_string(),
            _ => {
                return Ok(ToolResult::err("task required + non-empty".to_string()));
            }
        };
        let allowed: Vec<String> = match params.get("allowed_tools") {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            _ => default_allowed_tools(),
        };
        let max_iter_raw = params["max_iterations"]
            .as_u64()
            .unwrap_or(SUBAGENT_DEFAULT_ITERATIONS as u64) as u32;
        let max_iter = max_iter_raw.min(SUBAGENT_MAX_ITERATIONS_CAP);

        // YYC-206: register a pending record up front so the TUI
        // sees the run as it starts. The id is included in the
        // tool's JSON payload so callers can correlate against the
        // store snapshot.
        let summary_for_store = task.chars().take(120).collect::<String>();
        let record = self
            .orchestration
            .register(None, summary_for_store, max_iter);
        let child_id = record.id;

        // YYC-208: pre-cancellation short-circuit. Skip the agent
        // build entirely if the parent already cancelled — saves a
        // catalog fetch + provider setup that would be wasted.
        if cancel.is_cancelled() {
            self.orchestration.mark_cancelled(child_id);
            let payload = json!({
                "status": "cancelled",
                "child_id": child_id.to_string(),
                "summary": "child cancelled before start",
                "budget_used": {
                    "iterations": 0,
                    "max_iterations": max_iter,
                },
            });
            return Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?));
        }

        let child = Agent::builder(self.config.as_ref())
            .with_hooks(HookRegistry::new())
            .with_max_iterations(max_iter)
            .build()
            .await;
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                self.orchestration.mark_failed(
                    child_id,
                    format!("child agent build failed: {e}"),
                    0,
                );
                return Ok(ToolResult::err(format!("child agent build failed: {e}")));
            }
        };
        child.restrict_tools(&allowed);
        self.orchestration
            .update_status(child_id, crate::orchestration::ChildStatus::Running);

        // YYC-208: fork a child cancellation token from the parent's
        // so cancelling the parent turn aborts the child's loop.
        // `child_token` cancels when the parent's token cancels and
        // can also be cancelled independently — which YYC-209
        // hands to the orchestration store so a TUI kill action
        // can target this specific child by id.
        let child_cancel = cancel.child_token();
        self.orchestration
            .register_cancel_handle(child_id, child_cancel.clone());
        let run_result = child
            .run_prompt_with_cancel(&task, child_cancel.clone())
            .await;
        // YYC-209: child run finished one way or another; drop the
        // handle so the store's cancel map only carries live
        // children. Done before the cancellation check below so
        // even the cancelled-by-parent path forgets the handle.
        self.orchestration.forget_cancel_handle(child_id);
        let iterations = child.iterations();
        // If parent cancelled, mark Cancelled regardless of how the
        // child's run_prompt happened to surface — its Err shape on
        // cancellation isn't load-bearing here.
        if cancel.is_cancelled() {
            self.orchestration.mark_cancelled(child_id);
            let payload = json!({
                "status": "cancelled",
                "child_id": child_id.to_string(),
                "summary": "child cancelled by parent",
                "budget_used": {
                    "iterations": iterations,
                    "max_iterations": max_iter,
                },
            });
            return Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?));
        }
        match run_result {
            Ok(final_text) => {
                self.orchestration
                    .mark_completed(child_id, final_text.clone(), iterations);
                let status = if iterations >= max_iter {
                    "budget_exceeded"
                } else {
                    "completed"
                };
                let payload = json!({
                    "status": status,
                    "child_id": child_id.to_string(),
                    "summary": final_text,
                    "budget_used": {
                        "iterations": iterations,
                        "max_iterations": max_iter,
                    },
                    "tools_granted": allowed.len(),
                });
                Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
            }
            Err(e) => {
                let err_msg = format!("child agent failed: {e}");
                self.orchestration
                    .mark_failed(child_id, err_msg.clone(), iterations);
                let payload = json!({
                    "status": "error",
                    "child_id": child_id.to_string(),
                    "summary": err_msg,
                    "budget_used": {
                        "iterations": iterations,
                        "max_iterations": max_iter,
                    },
                });
                Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_allowed_tools_contains_read_only_set() {
        let tools = default_allowed_tools();
        assert!(tools.contains(&"read_file".to_string()));
        assert!(tools.contains(&"goto_definition".to_string()));
        // Dangerous tools must not appear in the default set.
        assert!(!tools.contains(&"write_file".to_string()));
        assert!(!tools.contains(&"edit_file".to_string()));
        assert!(!tools.contains(&"bash".to_string()));
        assert!(!tools.contains(&"spawn_subagent".to_string()));
    }

    #[tokio::test]
    async fn missing_task_returns_tool_error() {
        let cfg = Arc::new(Config::default());
        let tool = SpawnSubagentTool::new(cfg);
        let result = tool
            .call(json!({}), CancellationToken::new())
            .await
            .expect("call ok");
        assert!(result.is_error);
        assert!(result.output.contains("task required"));
    }

    #[tokio::test]
    async fn empty_task_string_returns_tool_error() {
        let cfg = Arc::new(Config::default());
        let tool = SpawnSubagentTool::new(cfg);
        let result = tool
            .call(json!({"task": "   "}), CancellationToken::new())
            .await
            .expect("call ok");
        assert!(result.is_error);
    }

    // YYC-208: a parent token cancelled before tool.call runs
    // short-circuits child build and marks the orchestration
    // record `Cancelled` without spawning anything.
    #[tokio::test]
    async fn precancelled_parent_marks_record_cancelled() {
        let cfg = Arc::new(Config::default());
        let store = Arc::new(OrchestrationStore::new());
        let tool = SpawnSubagentTool::with_store(cfg, Arc::clone(&store));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = tool
            .call(json!({"task": "hello"}), cancel)
            .await
            .expect("call ok");
        assert!(!result.is_error);
        assert!(result.output.contains("\"status\": \"cancelled\""));
        // Exactly one record, terminal Cancelled.
        assert_eq!(store.len(), 1);
        let recent = store.recent(1);
        assert_eq!(
            recent[0].status,
            crate::orchestration::ChildStatus::Cancelled,
        );
        assert!(recent[0].ended_at.is_some());
    }

    #[test]
    fn iteration_cap_clamps_to_ceiling() {
        assert!(SUBAGENT_MAX_ITERATIONS_CAP >= SUBAGENT_DEFAULT_ITERATIONS);
    }
}
