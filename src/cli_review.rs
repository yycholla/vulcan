//! YYC-190 PR-2: `vulcan review plan|diff|run` CLI driver.
//!
//! Loads the target text (file path, `-` for stdin, or
//! reconstructed from a run id), kicks off [`crate::review::run_review`]
//! under the `reviewer` capability profile, prints the rendered
//! markdown, and persists the report as a YYC-180 artifact when a
//! store is available.

use std::io::Read;

use anyhow::{Context, Result};

use crate::artifact::SqliteArtifactStore;
use crate::cli::ReviewSubcommand;
use crate::config::Config;
use crate::review::{ReviewKind, persist_report, run_review};
use crate::run_record::{RunStore, SqliteRunStore};

pub async fn run(cmd: ReviewSubcommand) -> Result<()> {
    let config = Config::load()?;
    match cmd {
        ReviewSubcommand::Plan { target } => {
            review(config, ReviewKind::Plan, load_target(&target)?).await
        }
        ReviewSubcommand::Diff { target } => {
            review(config, ReviewKind::Diff, load_target(&target)?).await
        }
        ReviewSubcommand::Run { id } => {
            let target = render_run_for_review(&id)?;
            review(config, ReviewKind::Run, target).await
        }
    }
}

async fn review(config: Config, kind: ReviewKind, target: String) -> Result<()> {
    let outcome = run_review(&config, kind.clone(), &target).await?;
    println!("{}", outcome.markdown);
    if outcome.report.has_blocking_finding() {
        eprintln!(
            "warning: review surfaced {} blocking finding(s)",
            outcome
                .report
                .findings
                .iter()
                .filter(|f| matches!(
                    f.severity,
                    crate::review::Severity::High | crate::review::Severity::Critical
                ))
                .count()
        );
    }
    if let Ok(store) = SqliteArtifactStore::try_new() {
        if let Some(id) = persist_report(Some(&store), &kind, &outcome, None)? {
            eprintln!("review report persisted as artifact {id}");
        }
    }
    Ok(())
}

fn load_target(spec: &str) -> Result<String> {
    if spec == "-" {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        return Ok(buf);
    }
    std::fs::read_to_string(spec).with_context(|| format!("read review target {spec}"))
}

fn render_run_for_review(raw_id: &str) -> Result<String> {
    let store = SqliteRunStore::try_new()?;
    let id = resolve_run_id_for_review(&store, raw_id)?;
    let rec = store
        .get(id)?
        .ok_or_else(|| anyhow::anyhow!("run record {id} not found"))?;
    let mut out = String::new();
    out.push_str(&format!(
        "Run {id} status={status} model={model}\n\n",
        status = rec.status_as_str(),
        model = rec.model.as_deref().unwrap_or("-"),
    ));
    out.push_str("## Events\n\n");
    for ev in &rec.events {
        out.push_str(&format!("- {ev:?}\n"));
    }
    Ok(out)
}

fn resolve_run_id_for_review(
    store: &SqliteRunStore,
    raw: &str,
) -> Result<crate::run_record::RunId> {
    if let Ok(uuid) = uuid::Uuid::parse_str(raw) {
        return Ok(crate::run_record::RunId::from_uuid(uuid));
    }
    let recent = store.recent(500)?;
    let matches: Vec<_> = recent
        .iter()
        .filter(|r| r.id.to_string().starts_with(raw))
        .collect();
    match matches.len() {
        0 => Err(anyhow::anyhow!("no run record matches `{raw}`")),
        1 => Ok(matches[0].id),
        n => Err(anyhow::anyhow!(
            "id prefix `{raw}` matched {n} runs — supply more characters"
        )),
    }
}

trait RunStatusDisplay {
    fn status_as_str(&self) -> &'static str;
}

impl RunStatusDisplay for crate::run_record::RunRecord {
    fn status_as_str(&self) -> &'static str {
        match self.status {
            crate::run_record::RunStatus::Created => "created",
            crate::run_record::RunStatus::Running => "running",
            crate::run_record::RunStatus::Completed => "completed",
            crate::run_record::RunStatus::Failed => "failed",
            crate::run_record::RunStatus::Cancelled => "cancelled",
        }
    }
}
