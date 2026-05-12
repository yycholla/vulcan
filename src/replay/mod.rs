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

use crate::artifact::{Artifact, ArtifactReplaySafety};
use crate::run_record::{RunEvent, RunId, RunRecord, RunStatus};
use crate::tools::profile::builtin_profile;

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

/// Structured summary of what can and cannot be reused from a saved
/// turn. This is deliberately diagnostic: it does not execute tools
/// or call providers, and it treats policy/capability checks as part
/// of replay rather than an obstacle to bypass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayReport {
    pub run_id: RunId,
    pub mode: ReplayMode,
    pub reused: Vec<String>,
    pub redacted: Vec<String>,
    pub missing: Vec<String>,
    pub policy_mismatches: Vec<String>,
    pub limits: Vec<String>,
}

impl ReplayReport {
    fn new(run_id: RunId, mode: ReplayMode) -> Self {
        Self {
            run_id,
            mode,
            reused: Vec::new(),
            redacted: Vec::new(),
            missing: Vec::new(),
            policy_mismatches: Vec::new(),
            limits: vec![
                "provider calls are nondeterministic; simulation reuses saved metadata, not exact model output".into(),
                "raw secrets, full environment snapshots, and unchecked tool payloads are never reconstructed".into(),
            ],
        }
    }
}

/// Build a replay simulation report from a saved run record plus typed
/// artifacts available for that run. The function is pure and never
/// executes tools or providers.
pub fn simulate(record: &RunRecord, artifacts: &[Artifact]) -> ReplayReport {
    let mut report = ReplayReport::new(record.id, ReplayMode::Mock);

    report.reused.push(format!(
        "run metadata status={}",
        format_status(record.status)
    ));
    if let Some(session) = &record.session_id {
        report.reused.push(format!("session id {session}"));
    }
    if let Some(model) = &record.model {
        report.reused.push(format!("recorded model {model}"));
    }

    let trust = last_trust_event(record);
    match trust {
        Some(TrustSnapshot {
            level,
            capability_profile,
            reason,
            allow_indexing,
            allow_persistence,
        }) => {
            report.reused.push(format!(
                "trust profile {level}/{capability_profile} indexing={allow_indexing} persistence={allow_persistence} ({reason})"
            ));
        }
        None => report.missing.push(
            "resolved trust/capability state missing; replay cannot verify tool policy".into(),
        ),
    }

    for event in &record.events {
        match event {
            RunEvent::PromptReceived {
                raw, char_count, ..
            } => {
                if raw.is_some() {
                    report
                        .reused
                        .push(format!("prompt body available ({char_count} chars)"));
                } else {
                    report.redacted.push(format!(
                        "prompt body redacted; only fingerprint and {char_count} chars remain"
                    ));
                }
            }
            RunEvent::ProviderRequest {
                model,
                streaming,
                message_count,
            } => {
                report.reused.push(format!(
                    "provider request metadata model={model} streaming={streaming} messages={message_count}"
                ));
            }
            RunEvent::ProviderResponse {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                finish_reason,
            } => {
                report.reused.push(format!(
                    "provider response usage p{prompt_tokens}/c{completion_tokens}/t{total_tokens} finish={}",
                    finish_reason.as_deref().unwrap_or("-")
                ));
            }
            RunEvent::ProviderError { message, retryable } => {
                report.reused.push(format!(
                    "provider error metadata retryable={retryable} message={message}"
                ));
            }
            RunEvent::ToolCall {
                name,
                error,
                is_error,
                ..
            } => {
                report.redacted.push(format!(
                    "tool {name} arguments redacted; only args fingerprint is available"
                ));
                if let Some(TrustSnapshot {
                    capability_profile, ..
                }) = trust
                {
                    match builtin_profile(capability_profile) {
                        Some(profile) if profile.allows(name) => report.reused.push(format!(
                            "tool {name} allowed by recorded profile {capability_profile}"
                        )),
                        Some(_) => report.policy_mismatches.push(format!(
                            "recorded profile {capability_profile} does not allow tool {name}"
                        )),
                        None => report.policy_mismatches.push(format!(
                            "recorded capability profile {capability_profile} is unavailable; cannot authorize tool {name}"
                        )),
                    }
                }
                if *is_error {
                    report.reused.push(format!(
                        "tool {name} completed as error: {}",
                        error.as_deref().unwrap_or("recorded error")
                    ));
                }
            }
            RunEvent::ArtifactCreated {
                artifact_id,
                artifact_type,
            } => {
                match artifacts
                    .iter()
                    .find(|artifact| artifact.id.to_string() == *artifact_id)
                {
                    Some(artifact) => classify_artifact(artifact, &mut report),
                    None => report.missing.push(format!(
                        "artifact {artifact_id} ({artifact_type}) not found in artifact store"
                    )),
                }
            }
            RunEvent::HookDecision {
                event,
                handler,
                outcome,
                detail,
            } => report.reused.push(format!(
                "hook decision {event}/{handler} -> {outcome}{}",
                detail
                    .as_deref()
                    .map(|d| format!(" ({d})"))
                    .unwrap_or_default()
            )),
            RunEvent::SubagentSpawned {
                child_run_id,
                task_summary,
            } => {
                report.limits.push(format!(
                    "subagent replay for child {child_run_id} ({task_summary}) is not included in this CLI/TUI slice"
                ));
            }
            RunEvent::StatusChanged { .. } | RunEvent::TrustResolved { .. } => {}
        }
    }

    report
}

pub fn render_report(report: &ReplayReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Replay simulation report: run {}\n\n",
        report.run_id
    ));
    out.push_str(&format!("- mode: {}\n", report.mode.as_str()));
    render_section(&mut out, "Reused context", &report.reused);
    render_section(&mut out, "Redacted/unavailable inputs", &report.redacted);
    render_section(&mut out, "Missing inputs/artifacts", &report.missing);
    render_section(
        &mut out,
        "Policy/capability mismatches",
        &report.policy_mismatches,
    );
    render_section(&mut out, "Reproduction limits", &report.limits);
    out
}

/// Run a non-Inspect replay. Stubbed for ToolReplay / Live execution;
/// Mock returns the simulation report so CLI/TUI users can reproduce
/// context without live provider or tool side effects.
pub fn run(record: &RunRecord, mode: ReplayMode) -> Result<String> {
    match mode {
        ReplayMode::Inspect => Err(anyhow!(
            "replay::run does not handle Inspect; call render_inspect directly"
        )),
        ReplayMode::Mock => Ok(render_report(&simulate(record, &[]))),
        other => Err(anyhow!(
            "replay mode `{}` requires live/tool execution and is not implemented in this CLI/TUI slice",
            other.as_str()
        )),
    }
}

#[derive(Debug, Clone, Copy)]
struct TrustSnapshot<'a> {
    level: &'a str,
    capability_profile: &'a str,
    reason: &'a str,
    allow_indexing: bool,
    allow_persistence: bool,
}

fn last_trust_event(record: &RunRecord) -> Option<TrustSnapshot<'_>> {
    record.events.iter().rev().find_map(|event| match event {
        RunEvent::TrustResolved {
            level,
            capability_profile,
            reason,
            allow_indexing,
            allow_persistence,
        } => Some(TrustSnapshot {
            level,
            capability_profile,
            reason,
            allow_indexing: *allow_indexing,
            allow_persistence: *allow_persistence,
        }),
        _ => None,
    })
}

fn classify_artifact(artifact: &Artifact, report: &mut ReplayReport) {
    let label = format!("artifact {} ({})", artifact.id, artifact.kind.as_str());
    match artifact.replay_safety {
        ArtifactReplaySafety::Safe => {
            if artifact.content.is_some()
                || artifact.storage_uri.is_some()
                || artifact.external_path.is_some()
            {
                report.reused.push(format!(
                    "{label} replay-safe via {}",
                    artifact
                        .storage_uri
                        .as_deref()
                        .or(artifact.external_path.as_deref())
                        .unwrap_or("inline content")
                ));
            } else {
                report.missing.push(format!(
                    "{label} marked safe but has no retrievable payload"
                ));
            }
        }
        ArtifactReplaySafety::SummaryOnly => report.redacted.push(format!(
            "{label} available for summary only; raw payload not reused"
        )),
        ArtifactReplaySafety::Unsafe => report.redacted.push(format!(
            "{label} marked unsafe for replay and will not be reused"
        )),
        ArtifactReplaySafety::Unknown => report.limits.push(format!(
            "{label} has unknown replay safety; treating as metadata only"
        )),
    }
}

fn render_section(out: &mut String, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    out.push_str(&format!("\n## {title}\n\n"));
    for item in items {
        out.push_str(&format!("- {item}\n"));
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
    fn mock_replay_returns_simulation_report() {
        let rec = fixture_record();
        let out = run(&rec, ReplayMode::Mock).unwrap();
        assert!(out.contains("# Replay simulation report"));
        assert!(out.contains("mode: mock"));
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

    #[test]
    fn simulation_reports_successful_reuse_and_recorded_policy() {
        let mut rec = fixture_record();
        rec.events.push(RunEvent::TrustResolved {
            level: "trusted".into(),
            capability_profile: "coding".into(),
            reason: "matched workspace".into(),
            allow_indexing: true,
            allow_persistence: true,
        });
        rec.events.push(RunEvent::ToolCall {
            name: "read_file".into(),
            args_fingerprint: PayloadFingerprint::of(b"{\"path\":\"README.md\"}"),
            approval: None,
            duration_ms: 10,
            is_error: false,
            error: None,
        });
        let artifact = crate::artifact::Artifact::inline_text(
            crate::artifact::ArtifactKind::Report,
            "replay-safe summary",
        )
        .with_run_id(rec.id)
        .with_replay_safety(crate::artifact::ArtifactReplaySafety::Safe);
        let report = simulate(&rec, &[artifact]);
        let txt = render_report(&report);

        assert!(txt.contains("# Replay simulation report"));
        assert!(txt.contains("trust profile trusted/coding"));
        assert!(txt.contains("tool read_file allowed by recorded profile coding"));
        assert!(!txt.contains("Policy/capability mismatches"));
    }

    #[test]
    fn simulation_reports_redacted_prompt_and_tool_args_without_raw_leak() {
        let mut rec = fixture_record();
        rec.events.push(RunEvent::ToolCall {
            name: "bash".into(),
            args_fingerprint: PayloadFingerprint::of(b"SECRET_TOKEN=abc123 cargo test"),
            approval: None,
            duration_ms: 10,
            is_error: false,
            error: None,
        });
        let report = simulate(&rec, &[]);
        let txt = render_report(&report);

        assert!(txt.contains("Redacted/unavailable inputs"));
        assert!(txt.contains("prompt body redacted"));
        assert!(txt.contains("tool bash arguments redacted"));
        assert!(!txt.contains("SECRET_TOKEN"));
        assert!(!txt.contains("abc123"));
    }

    #[test]
    fn simulation_reports_missing_artifact_references() {
        let mut rec = fixture_record();
        rec.events.push(RunEvent::ArtifactCreated {
            artifact_id: "missing-artifact".into(),
            artifact_type: "report".into(),
        });

        let report = simulate(&rec, &[]);
        let txt = render_report(&report);

        assert!(txt.contains("Missing inputs/artifacts"));
        assert!(txt.contains("artifact missing-artifact (report) not found"));
    }

    #[test]
    fn simulation_reports_policy_capability_mismatch() {
        let mut rec = fixture_record();
        rec.events.push(RunEvent::TrustResolved {
            level: "untrusted".into(),
            capability_profile: "readonly".into(),
            reason: "no matching workspace".into(),
            allow_indexing: false,
            allow_persistence: false,
        });
        rec.events.push(RunEvent::ToolCall {
            name: "bash".into(),
            args_fingerprint: PayloadFingerprint::of(b"cargo test"),
            approval: None,
            duration_ms: 10,
            is_error: false,
            error: None,
        });

        let report = simulate(&rec, &[]);
        let txt = render_report(&report);

        assert!(txt.contains("Policy/capability mismatches"));
        assert!(txt.contains("recorded profile readonly does not allow tool bash"));
    }
}
