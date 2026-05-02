//! YYC-179 PR-5: `vulcan run` CLI surface — list recent runs and
//! show the full timeline for a single run id.
//!
//! Reads from the same `~/.vulcan/run_records.db` the agent writes
//! to. No mutation; this is a query-only command.

use anyhow::{Context, Result, anyhow};
use owo_colors::OwoColorize;
use std::io::IsTerminal;
use uuid::Uuid;

use crate::cli::RunSubcommand;
use crate::run_record::{RunEvent, RunId, RunRecord, RunStatus, RunStore, SqliteRunStore};

pub async fn run(cmd: Option<RunSubcommand>) -> Result<()> {
    let store = SqliteRunStore::try_new().context("open ~/.vulcan/run_records.db")?;
    match cmd {
        None => interactive_select(&store),
        Some(RunSubcommand::List { limit }) => list(&store, limit),
        Some(RunSubcommand::Show { id }) => show(&store, &id),
    }
}

fn interactive_select(store: &SqliteRunStore) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "vulcan run (interactive) requires a terminal. Use `vulcan run list` to browse, or `vulcan run show <id>` to inspect a run."
        );
    }

    let recent = store.recent(20)?;
    if recent.is_empty() {
        println!("No run records yet.");
        return Ok(());
    }

    let labels: Vec<String> = recent.iter().map(run_picker_label).collect();
    println!();
    let pick = dialoguer::FuzzySelect::with_theme(&dialoguer::theme::ColorfulTheme::default())
        .with_prompt("Pick a run")
        .items(&labels)
        .default(0)
        .interact()
        .context("picker cancelled")?;

    print!("{}", render_run_show(&recent[pick]));
    Ok(())
}

fn list<S: RunStore + ?Sized>(store: &S, limit: usize) -> Result<()> {
    let recent = store.recent(limit)?;
    if recent.is_empty() {
        println!("No run records yet.");
        return Ok(());
    }
    print!("{}", render_run_list(&recent));
    Ok(())
}

fn show<S: RunStore + ?Sized>(store: &S, raw_id: &str) -> Result<()> {
    let id = resolve_run_id(store, raw_id)?;
    let rec = store
        .get(id)?
        .ok_or_else(|| anyhow!("run record {id} not found"))?;
    print!("{}", render_run_show(&rec));
    Ok(())
}

fn render_run_list(records: &[RunRecord]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<10} {:<12} {:<14} {:<18} {:<22} {:<10} {:<7} {}\n",
        "id".bold(),
        "status".bold(),
        "session".bold(),
        "tool".bold(),
        "timestamp".bold(),
        "duration".bold(),
        "turns".bold(),
        "model".bold(),
    ));
    for rec in records {
        out.push_str(&format!(
            "{:<10} {:<12} {:<14} {:<18} {:<22} {:<10} {:<7} {}\n",
            short_id(&rec.id.to_string()),
            status_badge(rec.status),
            truncate(rec.session_id.as_deref().unwrap_or("-"), 14),
            truncate(&tool_summary(rec), 18),
            rec.started_at.format("%Y-%m-%d %H:%M:%S"),
            duration_summary(rec),
            turn_count(rec),
            rec.model.as_deref().unwrap_or("-"),
        ));
    }
    out
}

fn render_run_show(rec: &RunRecord) -> String {
    let mut out = String::new();
    out.push_str(&format!("{} {}\n", "Run".bold(), rec.id));
    out.push_str(&format!("{}\n", "Metadata".bold().underline()));
    out.push_str(&format!("  status:     {}\n", status_badge(rec.status)));
    out.push_str(&format!("  origin:     {:?}\n", rec.origin));
    out.push_str(&format!(
        "  session:    {}\n",
        rec.session_id.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "  model:      {}\n",
        rec.model.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "  started:    {}\n",
        rec.started_at.format("%Y-%m-%d %H:%M:%S%.3f UTC")
    ));
    if let Some(end) = rec.ended_at {
        out.push_str(&format!(
            "  ended:      {}\n",
            end.format("%Y-%m-%d %H:%M:%S%.3f UTC")
        ));
    }
    out.push_str(&format!("  duration:   {}\n", duration_summary(rec)));
    out.push_str(&format!("  turns:      {}\n", turn_count(rec)));
    out.push_str(&format!("  tools:      {}\n", tool_summary(rec)));
    if let Some(err) = &rec.error {
        out.push_str(&format!("  error:      {}\n", err.red()));
    }
    out.push_str(&format!(
        "\n{} ({})\n",
        "Timeline".bold().underline(),
        rec.events.len()
    ));
    for (i, ev) in rec.events.iter().enumerate() {
        out.push_str(&format!("  [{i:>3}] {}\n", format_event(ev)));
    }
    out
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

fn status_badge(s: RunStatus) -> String {
    match s {
        RunStatus::Created => format_status(s).dimmed().to_string(),
        RunStatus::Running => format_status(s).blue().bold().to_string(),
        RunStatus::Completed => format_status(s).green().bold().to_string(),
        RunStatus::Failed => format_status(s).red().bold().to_string(),
        RunStatus::Cancelled => format_status(s).yellow().bold().to_string(),
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn duration_summary(rec: &RunRecord) -> String {
    let Some(ended) = rec.ended_at else {
        return "-".into();
    };
    let millis = (ended - rec.started_at).num_milliseconds().max(0);
    if millis < 1_000 {
        format!("{millis}ms")
    } else {
        format!("{:.2}s", millis as f64 / 1_000.0)
    }
}

fn turn_count(rec: &RunRecord) -> usize {
    rec.events
        .iter()
        .filter(|ev| matches!(ev, RunEvent::PromptReceived { .. }))
        .count()
}

fn tool_summary(rec: &RunRecord) -> String {
    let mut tools = rec.events.iter().filter_map(|ev| match ev {
        RunEvent::ToolCall { name, .. } => Some(name.as_str()),
        _ => None,
    });
    let Some(first) = tools.next() else {
        return "-".into();
    };
    let rest = tools.count();
    if rest == 0 {
        first.to_string()
    } else {
        format!("{first} +{rest}")
    }
}

fn run_picker_label(rec: &RunRecord) -> String {
    format!(
        "{}  {:<10}  {:<16}  {}",
        short_id(&rec.id.to_string()),
        format_status(rec.status),
        truncate(&tool_summary(rec), 16),
        rec.started_at.format("%Y-%m-%d %H:%M:%S")
    )
}

fn truncate(s: &str, max_chars: usize) -> String {
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars && max_chars > 1 {
        out.pop();
        out.push('…');
    }
    out
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
    use crate::run_record::{InMemoryRunStore, PayloadFingerprint, RunOrigin, RunRecord};

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

    #[test]
    fn render_run_list_includes_issue_542_columns() {
        let mut rec = RunRecord::new(RunOrigin::Cli);
        rec.session_id = Some("session-abc".into());
        rec.model = Some("gpt-5".into());
        rec.status = RunStatus::Completed;
        rec.ended_at = Some(rec.started_at + chrono::TimeDelta::milliseconds(1_250));
        rec.events.push(RunEvent::PromptReceived {
            fingerprint: PayloadFingerprint::of(b"hello"),
            char_count: 5,
            raw: None,
        });
        rec.events.push(RunEvent::ToolCall {
            name: "read_file".into(),
            args_fingerprint: PayloadFingerprint::of(b"{}"),
            approval: None,
            duration_ms: 41,
            is_error: false,
            error: None,
        });

        let out = render_run_list(&[rec]);

        assert!(out.contains("id"));
        assert!(out.contains("session"));
        assert!(out.contains("tool"));
        assert!(out.contains("duration"));
        assert!(out.contains("turns"));
        assert!(out.contains("session-abc"));
        assert!(out.contains("read_file"));
        assert!(out.contains("1.25s"));
        assert!(out.contains("1"));
    }

    #[test]
    fn render_run_show_sections_timeline() {
        let mut rec = RunRecord::new(RunOrigin::Cli);
        rec.status = RunStatus::Failed;
        rec.error = Some("provider unavailable".into());
        rec.events.push(RunEvent::ProviderError {
            message: "timeout".into(),
            retryable: true,
        });

        let out = render_run_show(&rec);

        assert!(out.contains("Run"));
        assert!(out.contains("Metadata"));
        assert!(out.contains("Timeline"));
        assert!(out.contains("failed"));
        assert!(out.contains("provider unavailable"));
        assert!(out.contains("provider error"));
    }
}
