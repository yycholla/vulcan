//! YYC-82: spawn_subagent tool — scoped child agent with budget.
//!
//! The parent agent dispatches a focused task to a child agent.
//! Execution requires a daemon runner, which delegates to daemon child
//! sessions. The child receives a restricted tool registry filtered to
//! an explicit allowlist, and the parent receives a bounded summary
//! plus budget usage stats, not a live transcript dump.
//!
//! ## Scope of this PR
//!
//! - Run child work behind the `SubagentRunner` seam when installed.
//! - Require daemon-backed child sessions for execution.
//! - Hard cap on loop iterations (`max_iterations`).
//! - Conservative default tool allowlist (read-only).
//! - Daemon child-session lineage when running under the daemon.
//!
//! ## Deliberately deferred
//!
//! - Token budget tracking (max_iterations is the proxy today).
//! - Transcript/artifact handle for inspection.
//! - TUI subagent tile (lands with YYC-68).
//!
//! Parent cancellation propagation lives at the call site:
//! `cancel.child_token()` derives a child token from the parent's,
//! so cancelling the parent's turn aborts the child's loop within
//! one iteration. YYC-209 also exposes the child-side handle through
//! the orchestration store so the TUI can target a specific child.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::orchestration::{ChildAgentId, OrchestrationStore};
use crate::tools::{Tool, ToolResult, parse_tool_params};

#[derive(Deserialize)]
struct SpawnSubagentParams {
    task: String,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    max_iterations: Option<u64>,
}

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

#[derive(Debug, Clone)]
pub struct SubagentRunRequest {
    pub child_id: ChildAgentId,
    pub parent_session_id: Option<String>,
    pub parent_run_id: Option<crate::run_record::RunId>,
    pub task: String,
    pub allowed_tools: Vec<String>,
    pub profile_name: Option<String>,
    pub max_iterations: u32,
}

#[derive(Debug, Clone)]
pub struct SubagentRunOutput {
    pub final_text: String,
    pub iterations: u32,
    pub tokens_consumed: u64,
}

#[async_trait]
pub trait SubagentRunner: Send + Sync {
    async fn run_subagent(
        &self,
        request: SubagentRunRequest,
        cancel: CancellationToken,
    ) -> Result<SubagentRunOutput>;
}

#[derive(Clone)]
pub struct SpawnSubagentTool {
    config: Arc<Config>,
    /// YYC-206: orchestration store the tool registers child runs
    /// against. Shared via `Arc` so the TUI / future admin endpoint
    /// can read the same state.
    orchestration: Arc<OrchestrationStore>,
    /// YYC-180: parent agent's artifact store. When the child run
    /// completes, the tool writes a `SubagentSummary` artifact here
    /// so the parent's `vulcan run show` view can link to it.
    artifact_store: Option<Arc<dyn crate::artifact::ArtifactStore>>,
    /// Slice 7 lineage: parent agent's session id, captured at
    /// build time. Stamped onto the orchestration record so the
    /// TUI / run viewer can link a child run back to the
    /// originating frontend session without joining run records
    /// against session metadata.
    parent_session_id: Option<String>,
    /// Slice 7 lineage: shared handle to parent agent's live
    /// `current_run_id`. Read at spawn time so the child's
    /// `RunOrigin::Subagent { parent_run_id }` carries the
    /// parent's in-flight run id, not a stale snapshot.
    parent_run_handle: Option<Arc<parking_lot::Mutex<Option<crate::run_record::RunId>>>>,
    /// Slice 7: when present, delegate child execution to a daemon
    /// child-session runner instead of building a direct child Agent.
    subagent_runner: Option<Arc<dyn SubagentRunner>>,
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
            artifact_store: None,
            parent_session_id: None,
            parent_run_handle: None,
            subagent_runner: None,
        }
    }

    /// Slice 7: install a shared handle to the parent agent's live
    /// `current_run_id`. The tool reads this at spawn time to stamp
    /// child runs with `RunOrigin::Subagent { parent_run_id }`.
    pub fn with_parent_run_handle(
        mut self,
        handle: Arc<parking_lot::Mutex<Option<crate::run_record::RunId>>>,
    ) -> Self {
        self.parent_run_handle = Some(handle);
        self
    }

    /// Slice 7: stamp the parent agent's session id so child
    /// orchestration records carry lineage back to the originating
    /// frontend session. Set at Agent build time — the parent's
    /// session id doesn't change across the parent's lifetime.
    pub fn with_parent_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.parent_session_id = Some(session_id.into());
        self
    }

    /// YYC-180: extra wiring — share the parent's artifact store so
    /// child-summary artifacts land alongside the parent's run.
    pub fn with_artifact_store(mut self, store: Arc<dyn crate::artifact::ArtifactStore>) -> Self {
        self.artifact_store = Some(store);
        self
    }

    pub fn with_subagent_runner(mut self, runner: Arc<dyn SubagentRunner>) -> Self {
        self.subagent_runner = Some(runner);
        self
    }
}

#[async_trait]
impl Tool for SpawnSubagentTool {
    fn name(&self) -> &str {
        "spawn_subagent"
    }

    fn description(&self) -> &str {
        "Delegate a focused task to a scoped child agent. The child runs an Agent loop with a restricted tool allowlist and a hard iteration cap, then returns a summary. Use for: reviewing a subsystem, summarizing a long file, comparing alternatives, bounded code search. Default tool set is read-only; pass `profile` for a named capability set, or `allowed_tools` to specify tool names directly."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The prompt the child agent will execute. Be specific — the child has no parent context."
                },
                "profile": {
                    "type": "string",
                    "description": "Named tool capability profile (YYC-181) — `readonly`, `coding`, `reviewer`, `gateway-safe`, or any user-defined `[tools.profiles.<name>]`. When set, supersedes `allowed_tools`."
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
        let p: SpawnSubagentParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let task = if p.task.trim().is_empty() {
            return Ok(ToolResult::err("task required + non-empty".to_string()));
        } else {
            p.task.clone()
        };
        // YYC-181: a named `profile` (built-in or user-defined in
        // `[tools.profiles]`) supersedes `allowed_tools`. Unknown
        // profile names surface as a tool error so the calling agent
        // sees a typed signal it can self-correct from.
        let profile_name = p.profile.clone();
        let allowed: Vec<String> = if let Some(name) = &profile_name {
            match self.config.tools.resolve_profile(name) {
                Some(prof) => prof.allowed.iter().map(|s| s.to_string()).collect(),
                None => {
                    return Ok(ToolResult::err(format!(
                        "unknown tool capability profile `{name}`. \
                         Built-in: readonly, coding, reviewer, gateway-safe."
                    )));
                }
            }
        } else {
            match p.allowed_tools.as_ref() {
                Some(arr) => arr.clone(),
                None => default_allowed_tools(),
            }
        };
        let max_iter_raw = p
            .max_iterations
            .unwrap_or(SUBAGENT_DEFAULT_ITERATIONS as u64) as u32;
        let max_iter = max_iter_raw.min(SUBAGENT_MAX_ITERATIONS_CAP);

        // YYC-206: register a pending record up front so the TUI
        // sees the run as it starts. The id is included in the
        // tool's JSON payload so callers can correlate against the
        // store snapshot.
        let summary_for_store = task.chars().take(120).collect::<String>();
        let record = self.orchestration.register_with_parent_session(
            None,
            self.parent_session_id.clone(),
            summary_for_store,
            max_iter,
        );
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
        let parent_run_id = self.parent_run_handle.as_ref().and_then(|h| *h.lock());
        if let Some(runner) = &self.subagent_runner {
            let request = SubagentRunRequest {
                child_id,
                parent_session_id: self.parent_session_id.clone(),
                parent_run_id,
                task: task.clone(),
                allowed_tools: allowed.clone(),
                profile_name: profile_name.clone(),
                max_iterations: max_iter,
            };
            let run_result = runner.run_subagent(request, child_cancel.clone()).await;
            self.orchestration.forget_cancel_handle(child_id);
            return self
                .finish_child_result(child_id, task, allowed.len(), max_iter, run_result, cancel)
                .await;
        }

        self.orchestration.forget_cancel_handle(child_id);
        let message = "SUBAGENT_REQUIRES_DAEMON: spawn_subagent requires daemon session wiring";
        self.orchestration.mark_failed(child_id, message, 0);
        Ok(ToolResult::err(message.to_string()))
    }
}

impl SpawnSubagentTool {
    async fn finish_child_result(
        &self,
        child_id: ChildAgentId,
        task: String,
        tools_granted: usize,
        max_iter: u32,
        run_result: Result<SubagentRunOutput>,
        cancel: CancellationToken,
    ) -> Result<ToolResult> {
        let (iterations, tokens_consumed) = match &run_result {
            Ok(out) => (out.iterations, out.tokens_consumed),
            Err(_) => (0, 0),
        };
        self.orchestration.update_tokens(child_id, tokens_consumed);
        if cancel.is_cancelled() {
            self.orchestration.mark_cancelled(child_id);
            let payload = json!({
                "status": "cancelled",
                "child_id": child_id.to_string(),
                "summary": "child cancelled by parent",
                "budget_used": {
                    "iterations": iterations,
                    "max_iterations": max_iter,
                    "tokens": tokens_consumed,
                },
            });
            return Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?));
        }
        match run_result {
            Ok(out) => {
                let final_text = out.final_text;
                self.orchestration
                    .mark_completed(child_id, final_text.clone(), iterations);
                // YYC-180: persist the child's final summary as a
                // typed artifact when the parent shared its store.
                // The artifact's `source` carries the child's id so
                // future replay can stitch parent-child timelines.
                if let Some(store) = &self.artifact_store {
                    let art = crate::artifact::Artifact::inline_text(
                        crate::artifact::ArtifactKind::SubagentSummary,
                        final_text.clone(),
                    )
                    .with_source(format!("subagent:{child_id}"))
                    .with_title(task.chars().take(60).collect::<String>());
                    if let Err(e) = store.create(&art) {
                        tracing::warn!("subagent summary artifact persist failed: {e}");
                    }
                }
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
                        "tokens": tokens_consumed,
                    },
                    "tools_granted": tools_granted,
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
                        "tokens": tokens_consumed,
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
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        // YYC-263: missing required field surfaces via parse_tool_params
        // as a serde-shaped "tool params failed to validate" message.
        assert!(result.output.contains("tool params failed to validate"));
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

    // ── YYC-181: named profile narrowing for subagents ───────────────

    #[tokio::test]
    async fn unknown_profile_returns_structured_error() {
        let cfg = Arc::new(Config::default());
        let tool = SpawnSubagentTool::new(cfg);
        let result = tool
            .call(
                json!({"task": "review", "profile": "imaginary"}),
                CancellationToken::new(),
            )
            .await
            .expect("call ok");
        assert!(result.is_error);
        assert!(
            result.output.contains("unknown tool capability profile"),
            "expected typed denial, got {:?}",
            result.output
        );
    }

    #[tokio::test]
    async fn missing_runner_requires_daemon() {
        let cfg = Arc::new(Config::default());
        let store = Arc::new(OrchestrationStore::new());
        let tool = SpawnSubagentTool::with_store(cfg, Arc::clone(&store));

        let result = tool
            .call(
                json!({"task": "inspect daemon-only execution"}),
                CancellationToken::new(),
            )
            .await
            .expect("call ok");

        assert!(result.is_error);
        assert!(result.output.contains("SUBAGENT_REQUIRES_DAEMON"));
        let recent = store.recent(1);
        assert_eq!(recent[0].status, crate::orchestration::ChildStatus::Failed);
        assert!(
            recent[0]
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("spawn_subagent requires daemon session")
        );
    }

    struct FakeRunner {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl SubagentRunner for FakeRunner {
        async fn run_subagent(
            &self,
            request: SubagentRunRequest,
            _cancel: CancellationToken,
        ) -> Result<SubagentRunOutput> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.parent_session_id.as_deref(), Some("parent-session"));
            assert_eq!(request.task, "inspect daemon");
            assert_eq!(request.max_iterations, 3);
            assert!(request.allowed_tools.contains(&"read_file".to_string()));
            Ok(SubagentRunOutput {
                final_text: "runner summary".into(),
                iterations: 2,
                tokens_consumed: 11,
            })
        }
    }

    #[tokio::test]
    async fn installed_runner_executes_subagent_without_child_builder() {
        let cfg = Arc::new(Config::default());
        let store = Arc::new(OrchestrationStore::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let runner = Arc::new(FakeRunner {
            calls: Arc::clone(&calls),
        });
        let tool = SpawnSubagentTool::with_store(cfg, Arc::clone(&store))
            .with_parent_session_id("parent-session")
            .with_subagent_runner(runner);

        let result = tool
            .call(
                json!({
                    "task": "inspect daemon",
                    "allowed_tools": ["read_file"],
                    "max_iterations": 3
                }),
                CancellationToken::new(),
            )
            .await
            .expect("call ok");

        assert!(!result.is_error, "runner path should succeed");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(result.output.contains("\"summary\": \"runner summary\""));
        let recent = store.recent(1);
        assert_eq!(
            recent[0].status,
            crate::orchestration::ChildStatus::Completed
        );
        assert_eq!(recent[0].iterations_used, 2);
        assert_eq!(recent[0].tokens_consumed, 11);
    }
}
