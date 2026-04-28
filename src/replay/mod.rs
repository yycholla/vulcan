//! YYC-184: replay + reproduce — render a saved run timeline
//! and (in later PRs) re-run it against mock or live providers.
//!
//! ## Scope of this PR
//!
//! - `ReplayMode` enum (Inspect / Mock / ToolReplay / Live).
//! - Inspect renderer: prints the saved run record's events
//!   without executing anything. Builds on top of YYC-179's
//!   `RunRecord`.
//! - Stub paths for Mock / ToolReplay / Live that return a
//!   typed "not yet implemented" error so callers see a clear
//!   contract.
//!
//! ## Deliberately deferred
//!
//! - Mock replay (re-run agent against recorded provider/tool
//!   outputs).
//! - Tool-replay (re-run pure tools while mocking the model).
//! - Live replay with explicit confirmation.
//! - Diff view between original and replay run records.

use anyhow::{Result, anyhow};

use crate::run_record::{RunEvent, RunRecord, RunStatus};

/// Replay execution mode. Inspect is the only path implemented
/// in this PR; the others land alongside their respective
/// runtime hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayMode {
    Inspect,
    Mock,
    ToolReplay,
    Live,
}

impl ReplayMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ReplayMode::Inspect => "inspect",
            ReplayMode::Mock => "mock",
            ReplayMode::ToolReplay => "tool-replay",
            ReplayMode::Live => "live",
        }
    }
}

/// Render a saved [`RunRecord`] as a human-readable timeline.
/// Pure function — no live re-execution. Stable shape so future
/// diff views can compare two renders.
pub fn render_inspect(record: &RunRecord) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Replay (inspect): run {}\n\n", record.id));
    out.push_str(&format!("- status: {}\n", format_status(record.status)));
    out.push_str(&format!(
        "- session: {}\n",
        record.session_id.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "- model: {}\n",
        record.model.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "- started: {}\n",
        record.started_at.format("%Y-%m-%d %H:%M:%S%.3f UTC")
    ));
    if let Some(end) = record.ended_at {
        out.push_str(&format!(
            "- ended: {}\n",
            end.format("%Y-%m-%d %H:%M:%S%.3f UTC")
        ));
    }
    if let Some(err) = &record.error {
        out.push_str(&format!("- error: {err}\n"));
    }
    out.push('\n');
    out.push_str(&format!("## Events ({})\n\n", record.events.len()));
    if record.events.is_empty() {
        out.push_str("_No events recorded._\n");
        return out;
    }
    for (idx, ev) in record.events.iter().enumerate() {
        out.push_str(&format!("[{idx:>3}] {}\n", format_event(ev)));
    }
    out
}

/// Run a non-Inspect replay. Stubbed in this PR — mock /
/// tool-replay / live execution land alongside their runtime
/// hooks. Returns a typed error so future PRs can replace this
/// without changing the call site shape.
pub fn run(_record: &RunRecord, mode: ReplayMode) -> Result<String> {
    match mode {
        ReplayMode::Inspect => Err(anyhow!(
            "replay::run does not handle Inspect; call render_inspect directly"
        )),
        other => Err(anyhow!(
            "replay mode `{}` is not yet implemented (stub from YYC-184 PR-1)",
            other.as_str()
        )),
    }
}

fn format_status(s: RunStatus) -> &'static str {
    match s {
        RunStatus::Created => "created",
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
    }
}

fn format_event(ev: &RunEvent) -> String {
    match ev {
        RunEvent::StatusChanged { status } => format!("status → {}", format_status(*status)),
        RunEvent::PromptReceived { char_count, .. } => {
            format!("prompt received ({char_count} chars, redacted)")
        }
        RunEvent::ProviderRequest {
            model,
            streaming,
            message_count,
        } => {
            format!("provider request model={model} streaming={streaming} messages={message_count}")
        }
        RunEvent::ProviderResponse {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            finish_reason,
        } => format!(
            "provider response tokens=p{prompt_tokens}/c{completion_tokens}/t{total_tokens} finish={}",
            finish_reason.as_deref().unwrap_or("-")
        ),
        RunEvent::ProviderError { message, retryable } => {
            format!("provider error retryable={retryable} msg={message}")
        }
        RunEvent::HookDecision {
            event,
            handler,
            outcome,
            detail,
        } => format!(
            "hook {event} {handler} → {outcome}{}",
            detail
                .as_deref()
                .map(|d| format!(" ({d})"))
                .unwrap_or_default()
        ),
        RunEvent::ToolCall {
            name,
            duration_ms,
            is_error,
            approval,
            ..
        } => format!(
            "tool {name} duration={duration_ms}ms{}{}",
            if *is_error { " ERROR" } else { "" },
            approval
                .as_deref()
                .map(|a| format!(" approval={a}"))
                .unwrap_or_default()
        ),
        RunEvent::SubagentSpawned {
            child_run_id,
            task_summary,
        } => format!("subagent {child_run_id} task={task_summary}"),
        RunEvent::ArtifactCreated {
            artifact_id,
            artifact_type,
        } => format!("artifact {artifact_type} id={artifact_id}"),
        RunEvent::TrustResolved {
            level,
            capability_profile,
            reason,
            allow_indexing,
            allow_persistence,
        } => format!(
            "trust level={level} capability={capability_profile} indexing={allow_indexing} persistence={allow_persistence} ({reason})"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_record::{
        InMemoryRunStore, PayloadFingerprint, RunEvent, RunOrigin, RunRecord, RunStore,
    };

    fn fixture_record() -> RunRecord {
        let mut rec = RunRecord::new(RunOrigin::Cli);
        rec.session_id = Some("sess-test".into());
        rec.model = Some("test-model".into());
        rec.events.push(RunEvent::StatusChanged {
            status: RunStatus::Running,
        });
        rec.events.push(RunEvent::PromptReceived {
            fingerprint: PayloadFingerprint::of(b"hello"),
            char_count: 5,
            raw: None,
        });
        rec.events.push(RunEvent::ProviderRequest {
            model: "test-model".into(),
            streaming: false,
            message_count: 2,
        });
        rec.events.push(RunEvent::ProviderResponse {
            prompt_tokens: 10,
            completion_tokens: 20,
            total_tokens: 30,
            finish_reason: Some("stop".into()),
        });
        rec.status = RunStatus::Completed;
        rec.ended_at = Some(chrono::Utc::now());
        rec
    }

    #[test]
    fn inspect_renders_redacted_prompt_and_event_list() {
        let rec = fixture_record();
        let txt = render_inspect(&rec);
        assert!(txt.contains("# Replay (inspect): run"));
        assert!(txt.contains("status: completed"));
        // Ensure raw prompt body never lands in the inspect output.
        assert!(!txt.contains("hello"));
        assert!(txt.contains("(5 chars, redacted)"));
        // Provider events surface tokens.
        assert!(txt.contains("p10/c20/t30"));
    }

    #[test]
    fn inspect_handles_empty_event_list() {
        let mut rec = fixture_record();
        rec.events.clear();
        let txt = render_inspect(&rec);
        assert!(txt.contains("_No events recorded._"));
    }

    #[test]
    fn mock_replay_returns_typed_not_implemented_error() {
        let rec = fixture_record();
        let err = run(&rec, ReplayMode::Mock).unwrap_err().to_string();
        assert!(err.contains("not yet implemented"));
        assert!(err.contains("mock"));
    }

    #[test]
    fn run_inspect_branch_directs_callers_to_render_inspect() {
        let rec = fixture_record();
        let err = run(&rec, ReplayMode::Inspect).unwrap_err().to_string();
        assert!(err.contains("render_inspect"));
    }

    #[test]
    fn inspect_round_trips_through_run_store() {
        // Ensures the fixture record actually round-trips through
        // the same path the live agent uses, so the inspect
        // renderer doesn't drift from real records.
        let store = InMemoryRunStore::default();
        let rec = fixture_record();
        store.create(&rec).unwrap();
        let got = store.get(rec.id).unwrap().unwrap();
        let txt = render_inspect(&got);
        assert!(txt.contains(&rec.id.to_string()));
    }
}
