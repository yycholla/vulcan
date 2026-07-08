use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

use crate::cli::SymphonySubcommand;
use crate::run_record::{RunOrigin, RunRecord, RunStatus, RunStore, SqliteRunStore};
use crate::symphony::config::{EffectiveConfig, TaskSourceKind};
use crate::symphony::orchestrator::{
    OrchestratorEvent, RunResult, SymphonyRuntimeSnapshot, SymphonySnapshotStatus,
};
use crate::symphony::runtime::{DryTickResult, RunOnceResult, SymphonyRuntime};
use crate::symphony::workflow::NormalizedTask;

pub async fn run(cmd: SymphonySubcommand) -> Result<()> {
    let output = match cmd {
        SymphonySubcommand::Create { guided, name } => {
            if guided {
                let name = if name.is_empty() {
                    "Symphony Workflow Builder".to_string()
                } else {
                    name.join(" ")
                };
                create_guided_to_string(&name)?
            } else if name.is_empty() {
                bail!(
                    "missing workflow name. Use `vulcan symphony create <name>` or `vulcan symphony create --guided`."
                );
            } else {
                create_to_string(&name.join(" "))?
            }
        }
        SymphonySubcommand::List => list_to_string()?,
        SymphonySubcommand::Validate { workflow } => validate_to_string(&workflow)?,
        SymphonySubcommand::Config { workflow } => config_to_string(&workflow)?,
        SymphonySubcommand::Tasks { workflow } => tasks_to_string(&workflow)?,
        SymphonySubcommand::Status { workflow } => status_to_string(workflow.as_deref())?,
        SymphonySubcommand::Tick { workflow } => tick_to_string(&workflow)?,
        SymphonySubcommand::RunOnce { workflow, fake } => run_once_to_string(&workflow, fake)?,
        SymphonySubcommand::Runs { limit, all } => runs_to_string(limit, all).await?,
    };
    print!("{output}");
    Ok(())
}

pub fn create_to_string(name: &str) -> Result<String> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    create_in_dir_to_string(name, &cwd)
}

pub fn create_in_dir_to_string(name: &str, root: &Path) -> Result<String> {
    let slug = workflow_slug(name)?;
    let options = WorkflowCreateOptions::markdown_defaults(slug);
    create_with_options_in_dir_to_string(options, root)
}

pub fn create_guided_to_string(name: &str) -> Result<String> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let slug = workflow_slug(name)?;
    let options = WorkflowCreateOptions::guided_spec_builder(slug);
    create_with_options_in_dir_to_string(options, &cwd)
}

pub fn create_guided_in_dir_to_string(name: &str, root: &Path) -> Result<String> {
    let slug = workflow_slug(name)?;
    let options = WorkflowCreateOptions::guided_spec_builder(slug);
    create_with_options_in_dir_to_string(options, root)
}

fn create_with_options_in_dir_to_string(
    options: WorkflowCreateOptions,
    root: &Path,
) -> Result<String> {
    let workflows_dir = workflows_dir(root);
    let tasks_dir = tasks_dir(root);
    fs::create_dir_all(&workflows_dir).with_context(|| {
        format!(
            "failed to create Symphony workflow directory `{}`",
            workflows_dir.display()
        )
    })?;
    fs::create_dir_all(&tasks_dir).with_context(|| {
        format!(
            "failed to create Symphony task directory `{}`",
            tasks_dir.display()
        )
    })?;

    let workflow = workflows_dir.join(format!("{}.md", options.slug));
    let tasks = tasks_dir.join(
        options
            .source_path
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("tasks.md")),
    );
    if workflow.exists() {
        bail!("Symphony workflow `{}` already exists", workflow.display());
    }
    if options.creates_task_file() && tasks.exists() {
        bail!("Symphony task file `{}` already exists", tasks.display());
    }

    fs::write(&workflow, workflow_template(&options))
        .with_context(|| format!("failed to write Symphony workflow `{}`", workflow.display()))?;
    if options.creates_task_file() {
        fs::write(&tasks, task_template(&options.slug))
            .with_context(|| format!("failed to write Symphony task file `{}`", tasks.display()))?;
    }

    let mut out = format!(
        "Created Symphony workflow `{}`\nworkflow: {}\n",
        options.slug,
        workflow.display()
    );
    if options.creates_task_file() {
        out.push_str(&format!("tasks: {}\n", tasks.display()));
    }
    out.push_str(&format!(
        "next: vulcan symphony validate {}\n",
        workflow.display()
    ));
    Ok(out)
}

pub fn list_to_string() -> Result<String> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    list_in_dir_to_string(&cwd)
}

pub fn list_in_dir_to_string(root: &Path) -> Result<String> {
    let workflows_dir = workflows_dir(root);
    if !workflows_dir.exists() {
        return Ok(empty_workflows_message());
    }

    let mut workflows = Vec::new();
    for entry in fs::read_dir(&workflows_dir).with_context(|| {
        format!(
            "failed to read Symphony workflow directory `{}`",
            workflows_dir.display()
        )
    })? {
        let entry = entry.context("failed to read Symphony workflow entry")?;
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "md") {
            workflows.push(path);
        }
    }
    workflows.sort();

    if workflows.is_empty() {
        return Ok(empty_workflows_message());
    }

    let mut out = String::from("Symphony workflows:\n");
    for workflow in workflows {
        let name = workflow
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("(unknown)");
        out.push_str(&format!("  {name} - {}\n", workflow.display()));
    }
    Ok(out)
}

pub fn validate_to_string(workflow: &Path) -> Result<String> {
    let runtime = SymphonyRuntime::load(workflow)?;
    Ok(render_config_summary(&runtime.validate().config))
}

pub fn config_to_string(workflow: &Path) -> Result<String> {
    validate_to_string(workflow)
}

pub fn tasks_to_string(workflow: &Path) -> Result<String> {
    let runtime = SymphonyRuntime::load(workflow)?;
    let result = runtime.list_tasks()?;
    Ok(render_task_list(&result.tasks))
}

fn render_task_list(tasks: &[NormalizedTask]) -> String {
    let mut out = String::new();
    if tasks.is_empty() {
        out.push_str("No eligible Symphony tasks.\n");
    } else {
        for task in tasks {
            out.push_str(&format_task(&task));
        }
    }
    out
}

pub fn tick_to_string(workflow: &Path) -> Result<String> {
    let runtime = SymphonyRuntime::load(workflow)?;
    let outcome = runtime.dry_tick(0)?;
    Ok(render_tick_outcome(&outcome))
}

pub fn status_to_string(workflow: Option<&Path>) -> Result<String> {
    let snapshot = if let Some(workflow) = workflow {
        SymphonyRuntime::load(workflow)?.snapshot(0)?
    } else {
        SymphonyRuntimeSnapshot::unavailable()
    };
    Ok(render_symphony_snapshot(&snapshot))
}

pub fn run_once_to_string(workflow: &Path, fake: bool) -> Result<String> {
    if !fake {
        bail!("`vulcan symphony run-once` currently requires --fake");
    }
    let runtime = SymphonyRuntime::load(workflow)?;
    let result = runtime.fake_run_once(0)?;
    Ok(render_run_once(&result))
}

pub fn slash_symphony_to_string(args: &str) -> Result<String> {
    let mut words = args.split_whitespace();
    match words.next() {
        None => Ok(symphony_manual_help()),
        Some("workflow" | "workflows" | "list") => list_to_string(),
        Some("validate") => {
            let workflow = required_slash_path(words.next(), "validate")?;
            validate_to_string(workflow.as_path())
        }
        Some("config") => {
            let workflow = required_slash_path(words.next(), "config")?;
            config_to_string(workflow.as_path())
        }
        Some("tasks") => {
            let workflow = required_slash_path(words.next(), "tasks")?;
            tasks_to_string(workflow.as_path())
        }
        Some("status") => {
            let workflow = words.next().map(PathBuf::from);
            status_to_string(workflow.as_deref())
        }
        Some("run-once") => {
            let mut workflow = None;
            let mut fake = false;
            for word in words {
                if word == "--fake" {
                    fake = true;
                } else if workflow.is_none() {
                    workflow = Some(PathBuf::from(word));
                }
            }
            let workflow = workflow
                .ok_or_else(|| anyhow::anyhow!("missing workflow path for /symphony run-once"))?;
            run_once_to_string(workflow.as_path(), fake)
        }
        Some(_) => Ok(symphony_manual_help()),
    }
}

pub async fn runs_to_string(limit: usize, all: bool) -> Result<String> {
    let store = SqliteRunStore::try_new().context("open ~/.vulcan/run_records.db")?;
    runs_from_store_to_string(&store, limit, all).await
}

async fn runs_from_store_to_string<S: RunStore + ?Sized>(
    store: &S,
    limit: usize,
    all: bool,
) -> Result<String> {
    let recent = store.recent(limit).await?;
    let runs = if all {
        recent
    } else {
        recent.into_iter().filter(is_symphony_run).collect()
    };
    Ok(render_symphony_runs(&runs, all))
}

fn is_symphony_run(record: &RunRecord) -> bool {
    match &record.origin {
        RunOrigin::Other(origin) if origin.contains("symphony") => true,
        _ => record
            .workspace
            .as_deref()
            .is_some_and(|workspace| workspace.contains(".symphony")),
    }
}

fn workflows_dir(root: &Path) -> PathBuf {
    root.join(".symphony").join("workflows")
}

fn tasks_dir(root: &Path) -> PathBuf {
    root.join(".symphony").join("tasks")
}

fn workflow_slug(name: &str) -> Result<String> {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if (ch.is_ascii_whitespace() || ch == '_' || ch == '-') && !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        bail!("Symphony workflow name must contain at least one ASCII letter or number");
    }
    Ok(slug)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkflowCreateOptions {
    slug: String,
    source_kind: GuidedTaskSource,
    source_path: PathBuf,
    active_states: Vec<String>,
    max_concurrent: usize,
    workspace_root: PathBuf,
    codex_profile: String,
    prompt_template: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuidedTaskSource {
    Markdown,
}

impl WorkflowCreateOptions {
    fn markdown_defaults(slug: String) -> Self {
        Self {
            source_path: PathBuf::from(format!("../tasks/{slug}.md")),
            source_kind: GuidedTaskSource::Markdown,
            active_states: vec!["ready-for-agent".into()],
            max_concurrent: 1,
            workspace_root: PathBuf::from("../workspaces"),
            codex_profile: "symphony".into(),
            prompt_template: "Handle {{ issue.identifier }}: {{ issue.title }}".into(),
            slug,
        }
    }

    fn guided_spec_builder(slug: String) -> Self {
        Self {
            source_path: PathBuf::from(format!("../tasks/{slug}.md")),
            source_kind: GuidedTaskSource::Markdown,
            active_states: vec!["ready-for-agent".into()],
            max_concurrent: 1,
            workspace_root: PathBuf::from("../workspaces"),
            codex_profile: "symphony".into(),
            prompt_template: guided_spec_prompt_template(),
            slug,
        }
    }

    fn creates_task_file(&self) -> bool {
        matches!(self.source_kind, GuidedTaskSource::Markdown)
    }
}

fn workflow_template(options: &WorkflowCreateOptions) -> String {
    let source_key = match options.source_kind {
        GuidedTaskSource::Markdown => "markdown",
    };
    let active_states = yaml_inline_list(&options.active_states);
    format!(
        r#"---
task_source:
  kind: {source_key}
  {source_key}:
    path: {source_path}
polling:
  active_states: {active_states}
  max_concurrent: {max_concurrent}
workspace:
  root: {workspace_root}
codex:
  command: codex
  args: ["--profile", "{codex_profile}"]
---
{prompt_template}
"#,
        source_path = options.source_path.display(),
        max_concurrent = options.max_concurrent,
        workspace_root = options.workspace_root.display(),
        codex_profile = options.codex_profile,
        prompt_template = options.prompt_template
    )
}

fn task_template(slug: &str) -> String {
    format!(
        r#"---
id: {slug}-spec
identifier: {slug}-spec
title: Human-in-the-loop Symphony workflow setup
state: ready-for-agent
labels: [symphony, workflow-spec]
body: |
  Use the symphony-workflow-setup skill to guide a human operator through a
  complete workflow spec before drafting or editing Symphony workflow files.
---
"#
    )
}

fn yaml_inline_list(values: &[String]) -> String {
    let items = values
        .iter()
        .map(|value| value.replace('"', "\\\""))
        .map(|value| format!("\"{value}\""))
        .collect::<Vec<_>>();
    format!("[{}]", items.join(", "))
}

fn truncate(s: &str, max_chars: usize) -> String {
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars && max_chars > 1 {
        out.pop();
        out.push('…');
    }
    out
}

fn guided_spec_prompt_template() -> String {
    r#"Use $symphony-workflow-setup for {{ issue.identifier }}: {{ issue.title }}.

Goal: guide the human operator through a full Symphony workflow setup, including spec, human gates, runtime contract, prompt contract, proposed workflow files, and verification commands.

Task context:
{{ issue.body }}

Do not write workflow files until the skill process reaches a complete spec and the human explicitly approves it.
"#
    .into()
}

fn empty_workflows_message() -> String {
    "No Symphony workflows found.\nCreate one with `/symphony create <name>` or `vulcan symphony create <name>`.\n".into()
}

fn render_config_summary(config: &EffectiveConfig) -> String {
    let mut out = String::new();
    out.push_str("Symphony workflow OK\n");
    out.push_str(&format!(
        "task_source: {}\n",
        task_source_kind(&config.task_source.kind)
    ));
    out.push_str(&format!(
        "active_states: {}\n",
        config.polling.active_states.join(", ")
    ));
    out.push_str(&format!(
        "terminal_states: {}\n",
        config.polling.terminal_states.join(", ")
    ));
    out.push_str(&format!(
        "max_concurrent: {}\n",
        config.polling.max_concurrent
    ));
    out.push_str(&format!(
        "workspace_root: {}\n",
        config.workspace.root.display()
    ));
    out.push_str(&format!(
        "preserve_success: {}\n",
        config.workspace.preserve_success
    ));
    out.push_str(&format!("codex: {}", config.codex.command));
    if !config.codex.args.is_empty() {
        out.push(' ');
        out.push_str(&config.codex.args.join(" "));
    }
    out.push('\n');
    out
}

fn task_source_kind(kind: &TaskSourceKind) -> &str {
    match kind {
        TaskSourceKind::GitHub => "github",
        TaskSourceKind::Markdown => "markdown",
        TaskSourceKind::Todo => "todo",
        TaskSourceKind::Other(value) => value,
    }
}

fn format_task(task: &NormalizedTask) -> String {
    let labels = if task.labels.is_empty() {
        String::new()
    } else {
        format!(" [{}]", task.labels.join(", "))
    };
    format!(
        "{} {} - {}{}\n",
        task.identifier, task.state, task.title, labels
    )
}

fn render_tick_outcome(outcome: &DryTickResult) -> String {
    let mut out = String::new();
    for event in &outcome.events {
        match event {
            OrchestratorEvent::CandidatesFetched { count } => {
                out.push_str(&format!("candidates={count}\n"));
            }
            OrchestratorEvent::Dispatched { id, run_id } => {
                let display_id = outcome.identifiers.get(id).unwrap_or(id);
                out.push_str(&format!("dispatched {display_id} run_id={run_id}\n"));
            }
            OrchestratorEvent::Requeued {
                id,
                next_at_ms,
                reason,
            } => {
                out.push_str(&format!(
                    "requeued {id} next_at_ms={next_at_ms} reason={reason:?}\n"
                ));
            }
            OrchestratorEvent::Released { id, reason } => {
                out.push_str(&format!("released {id} reason={reason:?}\n"));
            }
            OrchestratorEvent::RefreshFailed => out.push_str("refresh=failed\n"),
            OrchestratorEvent::StatusPublished => out.push_str("status=published\n"),
            OrchestratorEvent::Reconciled
            | OrchestratorEvent::ConfigValidated
            | OrchestratorEvent::Completed { .. } => {}
        }
    }
    out
}

fn render_run_once(result: &RunOnceResult) -> String {
    let mut out = render_tick_outcome(&result.tick);
    match &result.report {
        Some(report) => {
            out.push_str(&format!(
                "run_result={} reason={:?}\n",
                run_result_label(&report.result),
                report.terminal_reason
            ));
            if let RunResult::Succeeded {
                input_tokens,
                output_tokens,
                turn_count,
                ..
            } = &report.result
            {
                out.push_str(&format!(
                    "turn_count={turn_count} input_tokens={input_tokens} output_tokens={output_tokens}\n"
                ));
            }
        }
        None => out.push_str("run_result=not_dispatched\n"),
    }
    out.push_str(&render_symphony_snapshot(&result.snapshot));
    out
}

fn render_symphony_snapshot(snapshot: &SymphonyRuntimeSnapshot) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Symphony status: {}\n",
        snapshot_status_label(snapshot.status)
    ));
    out.push_str(&format!("running: {}\n", snapshot.running.len()));
    for row in &snapshot.running {
        out.push_str(&format!(
            "  {} {} run_id={} state={} runtime_secs={} last_seen_ms={}\n",
            row.task_identifier,
            row.task_id,
            row.run_id,
            row.state,
            row.runtime_secs,
            row.last_seen_ms
        ));
    }
    out.push_str(&format!("retrying: {}\n", snapshot.retrying.len()));
    for row in &snapshot.retrying {
        out.push_str(&format!(
            "  {} next_at_ms={} attempt={}\n",
            row.task_id, row.next_at_ms, row.attempt
        ));
    }
    out.push_str(&format!("turn_count: {}\n", snapshot.turn_count));
    out.push_str(&format!(
        "tokens: input={} output={}\n",
        snapshot.token_totals.input, snapshot.token_totals.output
    ));
    out.push_str(&format!(
        "live_runtime_secs: {}\n",
        snapshot.live_runtime_secs
    ));
    if snapshot.latest_rate_limits.is_empty() {
        out.push_str("rate_limits: none\n");
    } else {
        out.push_str("rate_limits:\n");
        for (name, limit) in &snapshot.latest_rate_limits {
            out.push_str(&format!(
                "  {name}: {}/{} reset_at_ms={}\n",
                limit.remaining,
                limit.limit,
                limit
                    .reset_at_ms
                    .map_or_else(|| "-".to_string(), |value| value.to_string())
            ));
        }
    }
    if let Some(health) = &snapshot.hook_health {
        out.push_str(&format!(
            "hooks: handlers={} errors={} timeouts={}\n",
            health.handler_count, health.errors, health.timeouts
        ));
    } else {
        out.push_str("hooks: unavailable\n");
    }
    out
}

fn run_result_label(result: &RunResult) -> &'static str {
    match result {
        RunResult::Succeeded { .. } => "succeeded",
        RunResult::NeedsContinuation => "needs_continuation",
        RunResult::Failed => "failed",
    }
}

fn snapshot_status_label(status: SymphonySnapshotStatus) -> &'static str {
    match status {
        SymphonySnapshotStatus::Ok => "ok",
        SymphonySnapshotStatus::Unavailable => "unavailable",
        SymphonySnapshotStatus::Timeout => "timeout",
    }
}

fn required_slash_path(value: Option<&str>, command: &str) -> Result<PathBuf> {
    value
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("missing workflow path for /symphony {command}"))
}

fn symphony_manual_help() -> String {
    [
        "Symphony manual E2E:",
        "  vulcan symphony validate <workflow>",
        "  vulcan symphony config <workflow>",
        "  vulcan symphony status [workflow]",
        "  vulcan symphony run-once <workflow> --fake",
        "  /symphony workflow",
        "  /symphony config <workflow>",
        "  /symphony status [workflow]",
        "  /symphony run-once <workflow> --fake",
        "",
    ]
    .join("\n")
}

fn render_symphony_runs(records: &[RunRecord], all: bool) -> String {
    if records.is_empty() {
        return if all {
            "No run records yet.\n".into()
        } else {
            "No Symphony run records found.\n".into()
        };
    }

    let mut out = String::new();
    out.push_str(&format!(
        "{:<10} {:<12} {:<18} {:<22} {:<10} {}\n",
        "id", "status", "origin", "timestamp", "duration", "workspace"
    ));
    for rec in records {
        out.push_str(&format!(
            "{:<10} {:<12} {:<18} {:<22} {:<10} {}\n",
            short_run_id(rec),
            run_status_label(rec.status),
            truncate(&run_origin_label(&rec.origin), 18),
            rec.started_at.format("%Y-%m-%d %H:%M:%S"),
            run_duration_summary(rec),
            rec.workspace.as_deref().unwrap_or("-"),
        ));
    }
    out
}

fn short_run_id(rec: &RunRecord) -> String {
    rec.id.to_string().chars().take(8).collect()
}

fn run_status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Created => "created",
        RunStatus::Running => "running",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
    }
}

fn run_origin_label(origin: &RunOrigin) -> String {
    match origin {
        RunOrigin::Cli => "cli".into(),
        RunOrigin::Tui => "tui".into(),
        RunOrigin::Gateway { lane } => format!("gateway:{lane}"),
        RunOrigin::Subagent { parent_run_id } => format!("subagent:{parent_run_id}"),
        RunOrigin::Other(value) => value.clone(),
    }
}

fn run_duration_summary(rec: &RunRecord) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn fixture_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("vulcan-symphony-cli-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create fixture dir");
        dir
    }

    fn write_workflow(dir: &Path) -> PathBuf {
        let tasks = dir.join("tasks.md");
        fs::write(
            &tasks,
            r#"---
id: task-1
identifier: ISSUE-1
title: Wire Symphony CLI
state: ready-for-agent
labels: [Symphony, ready-for-agent]
---
---
id: task-2
identifier: ISSUE-2
title: Finished task
state: done
---
"#,
        )
        .expect("write tasks");

        let workflow = dir.join("WORKFLOW.md");
        fs::write(
            &workflow,
            r#"---
task_source:
  kind: markdown
  markdown:
    path: tasks.md
polling:
  active_states: [ready-for-agent]
  max_concurrent: 1
codex:
  command: codex
  args: ["--profile", "symphony"]
---
Handle {{ issue.identifier }}: {{ issue.title }}
"#,
        )
        .expect("write workflow");
        workflow
    }

    #[test]
    fn validate_renders_effective_config_summary() {
        let dir = fixture_dir("validate");
        let workflow = write_workflow(&dir);

        let output = validate_to_string(&workflow).expect("validate workflow");

        assert!(output.contains("Symphony workflow OK"));
        assert!(output.contains("task_source: markdown"));
        assert!(output.contains("active_states: ready-for-agent"));
        assert!(output.contains("codex: codex --profile symphony"));
    }

    #[test]
    fn tasks_lists_only_active_candidates() {
        let dir = fixture_dir("tasks");
        let workflow = write_workflow(&dir);

        let output = tasks_to_string(&workflow).expect("list tasks");

        assert!(output.contains("ISSUE-1"));
        assert!(output.contains("Wire Symphony CLI"));
        assert!(!output.contains("ISSUE-2"));
    }

    #[tokio::test]
    async fn tick_runs_one_deterministic_dry_run_dispatch() {
        let dir = fixture_dir("tick");
        let workflow = write_workflow(&dir);

        let output = tick_to_string(&workflow).expect("run tick");

        assert!(output.contains("candidates=1"));
        assert!(output.contains("dispatched ISSUE-1"));
        assert!(output.contains("run_id=symphony-dry-run-1"));
        assert!(output.contains("status=published"));
    }

    #[test]
    fn manual_e2e_status_and_fake_run_once_use_snapshot_renderer() {
        let dir = fixture_dir("manual-e2e");
        let workflow = write_workflow(&dir);

        let status = status_to_string(Some(&workflow)).expect("status");
        assert!(status.contains("Symphony status: ok"));
        assert!(status.contains("turn_count: 0"));
        assert!(status.contains("hooks: unavailable"));

        let unavailable = status_to_string(None).expect("unavailable status");
        assert!(unavailable.contains("Symphony status: unavailable"));

        let output = run_once_to_string(&workflow, true).expect("fake run once");
        assert!(output.contains("dispatched ISSUE-1"));
        assert!(output.contains("run_result=succeeded"));
        assert!(output.contains("turn_count=1 input_tokens=1 output_tokens=1"));
        assert!(output.contains("Symphony status: ok"));
        assert!(output.contains("tokens: input=1 output=1"));
    }

    #[test]
    fn slash_symphony_commands_share_manual_e2e_renderers() {
        let dir = fixture_dir("slash-manual-e2e");
        let workflow = write_workflow(&dir);

        let help = slash_symphony_to_string("").expect("help");
        assert!(help.contains("vulcan symphony run-once <workflow> --fake"));

        let config =
            slash_symphony_to_string(&format!("config {}", workflow.display())).expect("config");
        assert!(config.contains("Symphony workflow OK"));

        let run = slash_symphony_to_string(&format!("run-once {} --fake", workflow.display()))
            .expect("run once");
        assert!(run.contains("run_result=succeeded"));
    }

    #[tokio::test]
    async fn runs_lists_symphony_associated_records_only_by_default() {
        let store = crate::run_record::InMemoryRunStore::default();
        let mut symphony = RunRecord::new(RunOrigin::Other("symphony".into()));
        symphony.workspace = Some(".symphony/workspaces/gh-1".into());
        let mut cli = RunRecord::new(RunOrigin::Cli);
        cli.workspace = Some("/repo".into());
        store.create(&symphony).await.unwrap();
        store.create(&cli).await.unwrap();

        let output = runs_from_store_to_string(&store, 20, false)
            .await
            .expect("list runs");

        assert!(output.contains(&symphony.id.to_string()[..8]));
        assert!(output.contains("symphony"));
        assert!(!output.contains(&cli.id.to_string()[..8]));
    }

    #[tokio::test]
    async fn runs_all_includes_non_symphony_records() {
        let store = crate::run_record::InMemoryRunStore::default();
        let cli = RunRecord::new(RunOrigin::Cli);
        store.create(&cli).await.unwrap();

        let output = runs_from_store_to_string(&store, 20, true)
            .await
            .expect("list runs");

        assert!(output.contains(&cli.id.to_string()[..8]));
        assert!(output.contains("cli"));
    }

    #[test]
    fn create_scaffolds_workflow_and_tasks_then_list_finds_it() {
        let dir = fixture_dir("create");

        let output = create_in_dir_to_string("Daily Triage", &dir).expect("create workflow");

        let workflow = dir.join(".symphony/workflows/daily-triage.md");
        let tasks = dir.join(".symphony/tasks/daily-triage.md");
        assert!(output.contains("Created Symphony workflow `daily-triage`"));
        assert!(workflow.exists());
        assert!(tasks.exists());
        assert!(
            fs::read_to_string(&workflow)
                .expect("read workflow")
                .contains("path: ../tasks/daily-triage.md")
        );

        let list = list_in_dir_to_string(&dir).expect("list workflows");
        assert!(list.contains("daily-triage"));
        assert!(list.contains(".symphony/workflows/daily-triage.md"));
        assert!(!list.contains(".symphony/tasks/daily-triage.md"));

        let validation = validate_to_string(&workflow).expect("validate scaffolded workflow");
        assert!(validation.contains("Symphony workflow OK"));
    }

    #[test]
    fn create_guided_scaffolds_agent_spec_builder() {
        let dir = fixture_dir("create-guided");

        let output =
            create_guided_in_dir_to_string("Workflow Builder", &dir).expect("create workflow");

        let workflow = dir.join(".symphony/workflows/workflow-builder.md");
        let tasks = dir.join(".symphony/tasks/workflow-builder.md");
        let workflow_body = fs::read_to_string(&workflow).expect("read workflow");
        let task_body = fs::read_to_string(&tasks).expect("read tasks");

        assert!(output.contains("Created Symphony workflow `workflow-builder`"));
        assert!(workflow_body.contains("Use $symphony-workflow-setup"));
        assert!(workflow_body.contains("human operator"));
        assert!(workflow_body.contains("human explicitly approves it"));
        assert!(task_body.contains("Human-in-the-loop Symphony workflow setup"));
        assert!(task_body.contains("symphony-workflow-setup skill"));

        let validation = validate_to_string(&workflow).expect("validate scaffolded workflow");
        assert!(validation.contains("Symphony workflow OK"));
    }

    #[test]
    fn guided_prompt_renders_for_seed_task() {
        let dir = fixture_dir("guided-render");
        create_guided_in_dir_to_string("Workflow Builder", &dir).expect("create workflow");

        let workflow = crate::symphony::workflow::Workflow::load(
            dir.join(".symphony/workflows/workflow-builder.md"),
        )
        .expect("load");
        let task = NormalizedTask {
            id: "workflow-builder-spec".into(),
            identifier: "workflow-builder-spec".into(),
            title: "Human-in-the-loop Symphony workflow setup".into(),
            body: "Spec body".into(),
            priority: None,
            state: "ready-for-agent".into(),
            branch: None,
            labels: vec!["symphony".into()],
            blockers: Vec::new(),
            url: None,
            path: None,
            created_at: None,
            updated_at: None,
            source: BTreeMap::new(),
        };

        let prompt = workflow
            .render_prompt(&crate::symphony::workflow::PromptInput {
                issue: task,
                attempt: None,
            })
            .expect("render guided prompt");

        assert!(prompt.contains("Use $symphony-workflow-setup"));
        assert!(prompt.contains("Human-in-the-loop Symphony workflow setup"));
        assert!(prompt.contains("human explicitly approves it"));
    }

    #[test]
    fn list_reports_empty_workflow_directory() {
        let dir = fixture_dir("list-empty");

        let output = list_in_dir_to_string(&dir).expect("list workflows");

        assert!(output.contains("No Symphony workflows found."));
        assert!(output.contains("/symphony create <name>"));
    }

    #[test]
    fn create_rejects_empty_slug() {
        let dir = fixture_dir("create-empty");

        let err = create_in_dir_to_string("!!!", &dir).expect_err("reject invalid name");

        assert!(err.to_string().contains("workflow name"));
    }
}
