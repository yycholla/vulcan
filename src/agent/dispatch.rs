//! Tool dispatch + result summarization helpers extracted from
//! `agent/mod.rs` (YYC-109 redo). Owns the `dispatch_tool` method
//! (impl Agent) plus the free helpers that summarize tool args /
//! results / outputs for the YYC-74 streaming card.

use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::hooks::ToolCallDecision;
use crate::run_record::{PayloadFingerprint, RunEvent};
use crate::tools::{ProgressSink, ToolResult};

use super::Agent;

impl Agent {
    /// Dispatch a single tool call, running BeforeToolCall + AfterToolCall
    /// hooks around it. Returns the result flattened to the `String` payload
    /// expected by `Message::Tool` (media references inlined as `[media: ...]`
    /// markers). Hooks see the full `ToolResult`.
    /// Run BeforeToolCall + tool dispatch + AfterToolCall hooks. Returns the
    /// final `ToolResult` so callers can both flatten it for `Message::Tool`
    /// and inspect `is_error` (e.g. to emit `StreamEvent::ToolCallEnd { ok }`).
    pub(in crate::agent) async fn dispatch_tool(
        &self,
        name: &str,
        raw_args: &str,
        cancel: CancellationToken,
        progress: Option<ProgressSink>,
    ) -> ToolResult {
        let parsed_args: Value = match serde_json::from_str(raw_args) {
            Ok(v) => v,
            Err(e) => {
                // Hooks see Null when args are unparseable, but we surface the
                // structured error to the LLM via `tools.execute` (which also
                // re-parses) so the agent can self-correct on the next turn.
                tracing::warn!(
                    "Tool '{name}' received unparseable JSON args ({e}). Raw: {raw_args}"
                );
                Value::Null
            }
        };

        // YYC-179: dispatch timing for the run-record duration field.
        let started = std::time::Instant::now();
        let args_fingerprint = PayloadFingerprint::of(raw_args.as_bytes());

        let (effective_args_str, blocked) = match self
            .hooks
            .before_tool_call(name, &parsed_args, cancel.clone())
            .await
        {
            ToolCallDecision::Continue => (raw_args.to_string(), None),
            ToolCallDecision::Block(reason) => (raw_args.to_string(), Some(reason)),
            ToolCallDecision::ReplaceArgs(new_args) => {
                // YYC-179: emit a hook decision event so the timeline
                // shows the args mutation distinctly from the tool call.
                self.record_run_event(RunEvent::HookDecision {
                    event: "before_tool_call".into(),
                    handler: "*replace_args*".into(),
                    outcome: "replace_args".into(),
                    detail: Some(name.to_string()),
                });
                (
                    serde_json::to_string(&new_args).unwrap_or_else(|_| raw_args.to_string()),
                    None,
                )
            }
        };

        let raw_result: ToolResult = if let Some(reason) = blocked.clone() {
            // YYC-179: record the block before failing the tool
            // call so dashboards can group denied dispatches.
            self.record_run_event(RunEvent::HookDecision {
                event: "before_tool_call".into(),
                handler: "*block*".into(),
                outcome: "block".into(),
                detail: Some(reason.clone()),
            });
            ToolResult::err(format!("Blocked: {reason}"))
        } else {
            match self
                .tools
                .execute_with_progress(name, &effective_args_str, cancel.clone(), progress)
                .await
            {
                Ok(r) => r,
                Err(e) => ToolResult::err(format!("Error: {e}")),
            }
        };

        let final_result = match self
            .hooks
            .after_tool_call(name, &raw_result, cancel.clone())
            .await
        {
            Some(replaced) => {
                self.record_run_event(RunEvent::HookDecision {
                    event: "after_tool_call".into(),
                    handler: "*replace_result*".into(),
                    outcome: "replace_result".into(),
                    detail: Some(name.to_string()),
                });
                replaced
            }
            None => raw_result,
        };

        // YYC-179: emit one ToolCall event per dispatch with the
        // duration, approval surface, and structured error flag.
        let duration_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let approval = if blocked.is_some() {
            Some("blocked".to_string())
        } else {
            None
        };
        let error = if final_result.is_error {
            Some(crate::run_record::PayloadFingerprint::of(
                final_result.output.as_bytes(),
            ))
            .map(|fp| fp.as_str().to_string())
        } else {
            None
        };
        self.record_run_event(RunEvent::ToolCall {
            name: name.to_string(),
            args_fingerprint,
            approval,
            duration_ms,
            is_error: final_result.is_error,
            error,
        });

        final_result
    }
}

pub(in crate::agent) fn summarize_tool_args(name: &str, raw_args: &str) -> Option<String> {
    let args: serde_json::Value = match serde_json::from_str(raw_args) {
        Ok(v) => v,
        Err(_) => return None,
    };
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let tail = |full: String, n: usize| -> String {
        if full.chars().count() <= n {
            full
        } else {
            let chars: Vec<char> = full.chars().collect();
            format!(
                "…{}",
                chars[chars.len() - n + 1..].iter().collect::<String>()
            )
        }
    };
    let truncate = |full: String, n: usize| -> String {
        if full.chars().count() <= n {
            full
        } else {
            let chars: Vec<char> = full.chars().collect();
            format!("{}…", chars[..n - 1].iter().collect::<String>())
        }
    };
    let summary = match name {
        // File tools — path is the salient bit.
        "read_file" | "write_file" | "edit_file" | "list_files" => s("path").map(|p| tail(p, 60)),
        // Search tools — pattern/query.
        "search_files" => s("pattern").map(|p| truncate(p, 60)),
        "code_query" => s("query").map(|q| truncate(q, 60)),
        "code_outline" | "code_extract" => s("path").map(|p| tail(p, 60)),
        "find_symbol" => s("name"),
        // Code semantics — file:line.
        "goto_definition" | "find_references" | "hover" | "diagnostics" | "type_definition"
        | "implementation" => {
            let p = s("path").map(|x| tail(x, 40));
            let line = args.get("line").and_then(|v| v.as_u64());
            match (p, line) {
                (Some(p), Some(l)) => Some(format!("{p}:{l}")),
                (Some(p), None) => Some(p),
                _ => None,
            }
        }
        // YYC-210: workspace symbol search — query + language.
        "workspace_symbol" => {
            let q = s("query").map(|x| truncate(x, 32));
            let lang = s("language");
            match (q, lang) {
                (Some(q), Some(l)) => Some(format!("{q} [{l}]")),
                (Some(q), None) => Some(q),
                _ => None,
            }
        }
        // YYC-210: call hierarchy — file:line + direction.
        "call_hierarchy" => {
            let p = s("path").map(|x| tail(x, 40));
            let line = args.get("line").and_then(|v| v.as_u64());
            let dir = s("direction").unwrap_or_else(|| "incoming".into());
            match (p, line) {
                (Some(p), Some(l)) => Some(format!("{p}:{l} ({dir})")),
                (Some(p), None) => Some(format!("{p} ({dir})")),
                _ => Some(dir),
            }
        }
        // YYC-210: code action — file:start_line.
        "code_action" => {
            let p = s("path").map(|x| tail(x, 40));
            let line = args.get("start_line").and_then(|v| v.as_u64());
            match (p, line) {
                (Some(p), Some(l)) => Some(format!("{p}:{l}")),
                (Some(p), None) => Some(p),
                _ => None,
            }
        }
        // YYC-210: spawn_subagent — task prefix.
        "spawn_subagent" => s("task").map(|t| truncate(t, 60)),
        "rename_symbol" => {
            let p = s("path").map(|x| tail(x, 40));
            let new = s("new_name");
            match (p, new) {
                (Some(p), Some(n)) => Some(format!("{p} → {n}")),
                (Some(p), None) => Some(p),
                _ => None,
            }
        }
        "replace_function_body" => {
            let p = s("path").map(|x| tail(x, 40));
            let sym = s("symbol");
            match (p, sym) {
                (Some(p), Some(sym)) => Some(format!("{p}::{sym}")),
                (Some(p), None) => Some(p),
                _ => None,
            }
        }
        // Web tools.
        "web_search" | "code_search_semantic" => s("query").map(|q| truncate(q, 60)),
        "web_fetch" => s("url").map(|u| truncate(u, 60)),
        // Shell tools.
        "bash" | "run_command" | "pty_create" | "pty_write" => {
            s("command").map(|c| truncate(c, 60))
        }
        "pty_read" | "pty_close" | "pty_resize" => s("session_id").map(|i| truncate(i, 16)),
        "pty_list" => Some("(all sessions)".into()),
        // Git tools.
        "git_status" | "git_log" | "git_diff" => Some(name.to_string()),
        "git_commit" => s("message").map(|m| truncate(m, 60)),
        "git_branch" => {
            let act = s("action").unwrap_or_else(|| "list".into());
            let nm = s("name");
            match nm {
                Some(n) => Some(format!("{act} {n}")),
                None => Some(act),
            }
        }
        "git_push" => {
            let r = s("remote").unwrap_or_else(|| "origin".into());
            let b = s("branch");
            match b {
                Some(b) => Some(format!("{r} {b}")),
                None => Some(r),
            }
        }
        "index_code_graph" | "index_code_embeddings" => Some("(workspace)".into()),
        _ => {
            // Generic: surface the first string-valued field.
            args.as_object().and_then(|o| {
                o.iter()
                    .find_map(|(_, v)| v.as_str().map(|s| truncate(s.to_string(), 60)))
            })
        }
    };
    summary.filter(|s| !s.is_empty())
}

/// One-line metadata about a tool result for the YYC-74 card sub-header
/// (e.g. "847 lines · 26.8 KB", "5 matches", "+12 -3"). Per-tool when
/// the output has structure; falls back to a generic line/char count.
pub(in crate::agent) fn summarize_tool_result(name: &str, output: &str) -> Option<String> {
    let text = output.trim();
    if text.is_empty() {
        return None;
    }
    let lines = text.lines().count();
    let bytes = text.len();
    let format_size = |b: usize| -> String {
        if b < 1024 {
            format!("{b} B")
        } else if b < 1024 * 1024 {
            format!("{:.1} KB", (b as f64) / 1024.0)
        } else {
            format!("{:.1} MB", (b as f64) / (1024.0 * 1024.0))
        }
    };
    match name {
        "write_file" => {
            // "Wrote N bytes to PATH"
            let bytes_n = text
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse::<usize>().ok());
            bytes_n.map(|n| format!("{} written", format_size(n)))
        }
        "edit_file" => {
            // "Replaced N occurrence(s) in PATH"
            let n = text
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse::<usize>().ok())?;
            Some(format!("{n} occurrence{}", if n == 1 { "" } else { "s" }))
        }
        "read_file" | "list_files" => Some(format!("{lines} lines · {}", format_size(bytes))),
        "search_files" => {
            // ripgrep output: each non-empty line is a hit
            let hits = text.lines().filter(|l| !l.is_empty()).count();
            Some(format!("{hits} match{}", if hits == 1 { "" } else { "es" }))
        }
        "git_log" => {
            let n = text.lines().filter(|l| !l.is_empty()).count();
            Some(format!("{n} commit{}", if n == 1 { "" } else { "s" }))
        }
        "git_diff" => {
            let plus = text
                .lines()
                .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
                .count();
            let minus = text
                .lines()
                .filter(|l| l.starts_with('-') && !l.starts_with("---"))
                .count();
            if plus == 0 && minus == 0 {
                None
            } else {
                Some(format!("+{plus} -{minus}"))
            }
        }
        "git_status" => {
            // First line is "## branch...origin/branch [ahead N]"; rest are file changes.
            let changes = text
                .lines()
                .filter(|l| !l.starts_with("##") && !l.is_empty())
                .count();
            if changes == 0 {
                Some("clean".to_string())
            } else {
                Some(format!(
                    "{changes} change{}",
                    if changes == 1 { "" } else { "s" }
                ))
            }
        }
        "code_outline" | "find_symbol" => {
            // JSON payloads — peek at the symbol/match counts.
            serde_json::from_str::<serde_json::Value>(text)
                .ok()
                .and_then(|v| {
                    let arr = v.get("symbols").or_else(|| v.get("matches"))?.as_array()?;
                    Some(format!(
                        "{} symbol{}",
                        arr.len(),
                        if arr.len() == 1 { "" } else { "s" }
                    ))
                })
        }
        "code_search_semantic" => serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|v| v.get("matches")?.as_array().map(|a| a.len()))
            .map(|n| format!("{n} hit{}", if n == 1 { "" } else { "s" })),
        "web_search" | "web_fetch" => Some(format!("{lines} lines · {}", format_size(bytes))),
        "bash" | "run_command" => Some(format!("{lines} lines · {}", format_size(bytes))),
        "diagnostics" => serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|v| v.get("count")?.as_u64())
            .map(|n| format!("{n} diagnostic{}", if n == 1 { "" } else { "s" })),
        // YYC-210: workspace_symbol — JSON `count`.
        "workspace_symbol" => serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|v| v.get("count")?.as_u64())
            .map(|n| format!("{n} symbol{}", if n == 1 { "" } else { "s" })),
        // YYC-210: type_definition / implementation — count
        // entries in `locations` / `implementations`.
        "type_definition" | "implementation" => serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|v| {
                let arr = v
                    .get("locations")
                    .or_else(|| v.get("implementations"))?
                    .as_array()?;
                Some(format!(
                    "{} hit{}",
                    arr.len(),
                    if arr.len() == 1 { "" } else { "s" }
                ))
            }),
        // YYC-210: call_hierarchy — count + direction.
        "call_hierarchy" => serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|v| {
                let count = v.get("count")?.as_u64()?;
                let dir = v.get("direction").and_then(|d| d.as_str()).unwrap_or("");
                Some(format!(
                    "{count} {} call{}",
                    dir,
                    if count == 1 { "" } else { "s" }
                ))
            }),
        // YYC-210: code_action — JSON `count`.
        "code_action" => serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|v| v.get("count")?.as_u64())
            .map(|n| format!("{n} action{}", if n == 1 { "" } else { "s" })),
        // YYC-210 / YYC-211: spawn_subagent — status + iterations + tokens.
        "spawn_subagent" => serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|v| {
                let status = v.get("status")?.as_str()?;
                let used = v
                    .get("budget_used")
                    .and_then(|b| b.get("iterations"))
                    .and_then(|i| i.as_u64())
                    .unwrap_or(0);
                let max = v
                    .get("budget_used")
                    .and_then(|b| b.get("max_iterations"))
                    .and_then(|i| i.as_u64())
                    .unwrap_or(0);
                let tokens = v
                    .get("budget_used")
                    .and_then(|b| b.get("tokens"))
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0);
                if tokens > 0 {
                    Some(format!("{status} · {used}/{max} iters · {tokens} tok"))
                } else {
                    Some(format!("{status} · {used}/{max} iters"))
                }
            }),
        // Generic fallback so even an unknown tool gets *something*.
        _ => {
            if lines == 1 {
                Some(format_size(bytes))
            } else {
                Some(format!("{lines} lines · {}", format_size(bytes)))
            }
        }
    }
}

/// Truncated tool result for the YYC-74 card preview block — caps at
/// ~12 lines / 1 KB. The full output still goes to the LLM via
/// `Message::Tool`; this is purely for rendering.
pub(in crate::agent) fn preview_output(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let chars: Vec<char> = trimmed.chars().take(1024).collect();
    let head: String = chars.iter().collect();
    let lines: Vec<&str> = head.lines().take(12).collect();
    Some(lines.join("\n"))
}

/// Number of full output lines hidden by `preview_output` (YYC-78).
/// Used by the card to render `… N more lines elided` when the
/// result was clipped.
pub(in crate::agent) fn elided_lines(text: &str, preview: Option<&str>) -> usize {
    let total = text.trim().lines().count();
    let shown = preview.map(|p| p.lines().count()).unwrap_or(0);
    total.saturating_sub(shown)
}
