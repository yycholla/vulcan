//! YYC-179 PR-5: `vulcan run` CLI surface — list recent runs and
//! show the full timeline for a single run id.
//!
//! Reads from the same `~/.vulcan/run_records.db` the agent writes
//! to. No mutation; this is a query-only command.

use anyhow::{Context, Result, anyhow};
use uuid::Uuid;

use crate::cli::RunSubcommand;
use crate::run_record::{RunEvent, RunId, RunStatus, RunStore, SqliteRunStore};

pub async fn run(cmd: RunSubcommand) -> Result<()> {
    let store = SqliteRunStore::try_new().context("open ~/.vulcan/run_records.db")?;
    match cmd {
        RunSubcommand::List { limit } => list(&store, limit),
        RunSubcommand::Show { id } => show(&store, &id),
    }
}

fn list(store: &SqliteRunStore, limit: usize) -> Result<()> {
    let recent = store.recent(limit)?;
    if recent.is_empty() {
        println!("No run records yet.");
        return Ok(());
    }
    println!(
        "{:<10} {:<12} {:<22} {:<22} model",
        "id", "status", "started", "ended"
    );
    for rec in recent {
        let id_short: String = rec.id.to_string().chars().take(8).collect();
        let started = rec.started_at.format("%Y-%m-%d %H:%M:%S");
        let ended = rec
            .ended_at
            .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "-".into());
        let status = format_status(rec.status);
        let model = rec.model.unwrap_or_else(|| "-".into());
        println!("{id_short:<10} {status:<12} {started:<22} {ended:<22} {model}");
    }
    Ok(())
}

fn show(store: &SqliteRunStore, raw_id: &str) -> Result<()> {
    let id = resolve_run_id(store, raw_id)?;
    let rec = store
        .get(id)?
        .ok_or_else(|| anyhow!("run record {id} not found"))?;
    println!("Run {id}");
    println!("  status:     {}", format_status(rec.status));
    println!("  origin:     {:?}", rec.origin);
    println!("  session:    {}", rec.session_id.as_deref().unwrap_or("-"));
    println!("  model:      {}", rec.model.as_deref().unwrap_or("-"));
    println!(
        "  started:    {}",
        rec.started_at.format("%Y-%m-%d %H:%M:%S%.3f UTC")
    );
    if let Some(end) = rec.ended_at {
        println!("  ended:      {}", end.format("%Y-%m-%d %H:%M:%S%.3f UTC"));
    }
    if let Some(err) = &rec.error {
        println!("  error:      {err}");
    }
    println!("\n  events ({}):", rec.events.len());
    for (i, ev) in rec.events.iter().enumerate() {
        println!("    [{i:>3}] {}", format_event(ev));
    }
    Ok(())
}

/// Accept either a full UUID or an 8-char prefix. Prefix lookup
/// scans recent runs (a few hundred at most) — fine for the size
/// of this store. Generic over `RunStore` so unit tests can drive
/// it with the in-memory backend.
fn resolve_run_id<S: RunStore + ?Sized>(store: &S, raw_id: &str) -> Result<RunId> {
    if let Ok(uuid) = Uuid::parse_str(raw_id) {
        return Ok(RunId::from_uuid(uuid));
    }
    let recent = store.recent(500)?;
    let matches: Vec<_> = recent
        .iter()
        .filter(|r| r.id.to_string().starts_with(raw_id))
        .collect();
    match matches.len() {
        0 => Err(anyhow!("no run record matches `{raw_id}`")),
        1 => Ok(matches[0].id),
        n => Err(anyhow!(
            "id prefix `{raw_id}` matched {n} runs — supply more characters"
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
    use crate::run_record::{InMemoryRunStore, RunOrigin, RunRecord};

    #[test]
    fn resolve_run_id_accepts_full_uuid() {
        let store = InMemoryRunStore::default();
        let rec = RunRecord::new(RunOrigin::Cli);
        let id = rec.id;
        store.create(&rec).unwrap();
        let resolved = resolve_run_id(&store, &id.to_string()).unwrap();
        assert_eq!(resolved, id);
    }

    #[test]
    fn resolve_run_id_accepts_prefix() {
        let store = InMemoryRunStore::default();
        let rec = RunRecord::new(RunOrigin::Cli);
        let id = rec.id;
        store.create(&rec).unwrap();
        let prefix: String = id.to_string().chars().take(8).collect();
        let resolved = resolve_run_id(&store, &prefix).unwrap();
        assert_eq!(resolved, id);
    }

    #[test]
    fn resolve_run_id_errors_on_no_match() {
        let store = InMemoryRunStore::default();
        let err = resolve_run_id(&store, "abcd1234").unwrap_err();
        assert!(err.to_string().contains("no run record matches"));
    }

    #[test]
    fn format_event_redacts_prompt_payload() {
        // YYC-179 acceptance: the timeline view must not print raw
        // prompt text. format_event for PromptReceived only shows
        // the char count.
        let ev = RunEvent::PromptReceived {
            fingerprint: crate::run_record::PayloadFingerprint::of(b"super secret"),
            char_count: 12,
            raw: None,
        };
        let line = format_event(&ev);
        assert!(line.contains("12 chars"));
        assert!(line.contains("redacted"));
        assert!(!line.contains("super secret"));
    }
}
