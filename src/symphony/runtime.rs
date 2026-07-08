//! Runtime facade for Symphony CLI and future service callers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::symphony::app_server::{AppServerOutcome, AppServerRequest, AppServerTelemetry};
use crate::symphony::config::{ConfigView, EffectiveConfig};
use crate::symphony::orchestrator::{
    Orchestrator, OrchestratorEvent, RunnerStart, RunnerStartResult, SymphonyRuntimeSnapshot,
    TaskRunner,
};
use crate::symphony::runner::{AgentRunReport, AgentRunRequest, AgentRunner, AgentWorker};
use crate::symphony::task_source::task_source_from_config;
use crate::symphony::workflow::{NormalizedTask, Workflow};
use crate::symphony::workspace::WorkspaceManager;

#[derive(Debug, Clone)]
pub struct SymphonyRuntime {
    workflow: Workflow,
    config: EffectiveConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationResult {
    pub config: EffectiveConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskListResult {
    pub tasks: Vec<NormalizedTask>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryTickResult {
    pub events: Vec<OrchestratorEvent>,
    pub identifiers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOnceResult {
    pub tick: DryTickResult,
    pub report: Option<AgentRunReport>,
    pub snapshot: SymphonyRuntimeSnapshot,
}

impl SymphonyRuntime {
    pub fn load(workflow: impl AsRef<Path>) -> Result<Self> {
        let workflow = workflow.as_ref();
        let workflow_doc = Workflow::load(workflow)
            .with_context(|| format!("failed to load workflow `{}`", workflow.display()))?;
        let repo_root = workflow
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let config = ConfigView::new(&workflow_doc.config, repo_root)
            .startup_validate()
            .context("invalid Symphony workflow config")?;
        Ok(Self {
            workflow: workflow_doc,
            config,
        })
    }

    pub fn validate(&self) -> ValidationResult {
        ValidationResult {
            config: self.config.clone(),
        }
    }

    pub fn list_tasks(&self) -> Result<TaskListResult> {
        let source = task_source_from_config(&self.config.task_source)?;
        let mut tasks = source.fetch_candidates(&self.config.polling.active_states)?;
        tasks.sort_by(|left, right| left.identifier.cmp(&right.identifier));
        Ok(TaskListResult { tasks })
    }

    pub fn dry_tick(&self, now_ms: u64) -> Result<DryTickResult> {
        let source = task_source_from_config(&self.config.task_source)?;
        let identifiers = source
            .fetch_candidates(&self.config.polling.active_states)?
            .into_iter()
            .map(|task| (task.id, task.identifier))
            .collect::<BTreeMap<_, _>>();
        let runner = DryRunRunner::default();
        let mut orchestrator = Orchestrator::new(self.config.polling.clone(), source, runner);
        let outcome = orchestrator.poll_tick(now_ms);
        Ok(DryTickResult {
            events: outcome.events,
            identifiers,
        })
    }

    pub fn snapshot(&self, now_ms: u64) -> Result<SymphonyRuntimeSnapshot> {
        let source = task_source_from_config(&self.config.task_source)?;
        let runner = DryRunRunner::default();
        let orchestrator = Orchestrator::new(self.config.polling.clone(), source, runner);
        Ok(orchestrator.snapshot(now_ms, None))
    }

    pub fn fake_run_once(&self, now_ms: u64) -> Result<RunOnceResult> {
        let source = task_source_from_config(&self.config.task_source)?;
        let mut tasks = source.fetch_candidates(&self.config.polling.active_states)?;
        tasks.sort_by(|left, right| left.identifier.cmp(&right.identifier));
        let identifiers = tasks
            .iter()
            .map(|task| (task.id.clone(), task.identifier.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut orchestrator =
            Orchestrator::new(self.config.polling.clone(), source, DryRunRunner::default());
        let outcome = orchestrator.poll_tick(now_ms);
        let dispatched = outcome.events.iter().find_map(|event| match event {
            OrchestratorEvent::Dispatched { id, .. } => Some(id.clone()),
            _ => None,
        });
        let tick = DryTickResult {
            events: outcome.events,
            identifiers,
        };

        let Some(id) = dispatched else {
            return Ok(RunOnceResult {
                tick,
                report: None,
                snapshot: orchestrator.snapshot(now_ms, None),
            });
        };
        let Some(task) = tasks.into_iter().find(|task| task.id == id) else {
            return Ok(RunOnceResult {
                tick,
                report: None,
                snapshot: orchestrator.snapshot(now_ms, None),
            });
        };

        let workspace =
            WorkspaceManager::from_config(self.config.workspace.clone(), self.config.hooks.clone());
        let worker = FakeAppServerWorker;
        let mut runner = AgentRunner::new(
            self.workflow.clone(),
            workspace,
            self.config.agent.clone(),
            worker,
        );
        let report = runner.run_attempt(AgentRunRequest { task, attempt: 1 });
        orchestrator.record_run_result(&id, report.result.clone(), now_ms + 1_000);
        Ok(RunOnceResult {
            tick,
            report: Some(report),
            snapshot: orchestrator.snapshot(now_ms + 1_000, None),
        })
    }
}

#[derive(Debug, Default)]
struct DryRunRunner {
    next: u64,
}

impl TaskRunner for DryRunRunner {
    fn start(&mut self, _task: &NormalizedTask, _now_ms: u64) -> RunnerStartResult {
        self.next += 1;
        RunnerStartResult::Started(RunnerStart {
            run_id: format!("symphony-dry-run-{}", self.next),
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct FakeAppServerWorker;

impl AgentWorker for FakeAppServerWorker {
    type Error = std::convert::Infallible;

    fn run_turn(
        &mut self,
        request: AppServerRequest,
    ) -> std::result::Result<AppServerOutcome, Self::Error> {
        Ok(AppServerOutcome::Completed(AppServerTelemetry {
            session_id: format!("{}:fake", request.task.identifier),
            input_tokens: 1,
            output_tokens: 1,
            messages: Vec::new(),
            rate_limits: Vec::new(),
        }))
    }

    fn cleanup(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use crate::symphony::config::TaskSourceKind;

    fn fixture_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vulcan-symphony-runtime-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create fixture dir");
        dir
    }

    fn write_workflow(dir: &Path) -> PathBuf {
        let tasks = dir.join("tasks.md");
        fs::write(
            &tasks,
            r#"---
id: task-b
identifier: ISSUE-B
title: Later task
state: ready-for-agent
---
---
id: task-a
identifier: ISSUE-A
title: First task
state: ready-for-agent
---
---
id: task-done
identifier: ISSUE-DONE
title: Done task
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
    fn validate_returns_effective_config() {
        let dir = fixture_dir("validate");
        let workflow = write_workflow(&dir);

        let runtime = SymphonyRuntime::load(&workflow).expect("load runtime");
        let result = runtime.validate();

        assert!(matches!(
            result.config.task_source.kind,
            TaskSourceKind::Markdown
        ));
        assert_eq!(result.config.polling.active_states, ["ready-for-agent"]);
        assert_eq!(result.config.codex.command, "codex");
    }

    #[test]
    fn list_tasks_returns_sorted_active_candidates() {
        let dir = fixture_dir("tasks");
        let workflow = write_workflow(&dir);

        let runtime = SymphonyRuntime::load(&workflow).expect("load runtime");
        let result = runtime.list_tasks().expect("list tasks");

        let identifiers = result
            .tasks
            .iter()
            .map(|task| task.identifier.as_str())
            .collect::<Vec<_>>();
        assert_eq!(identifiers, ["ISSUE-A", "ISSUE-B"]);
    }

    #[test]
    fn dry_tick_returns_events_and_identifier_map() {
        let dir = fixture_dir("tick");
        let workflow = write_workflow(&dir);

        let runtime = SymphonyRuntime::load(&workflow).expect("load runtime");
        let result = runtime.dry_tick(0).expect("dry tick");

        assert!(
            result
                .events
                .contains(&OrchestratorEvent::CandidatesFetched { count: 2 })
        );
        assert!(result.events.contains(&OrchestratorEvent::Dispatched {
            id: "task-a".into(),
            run_id: "symphony-dry-run-1".into(),
        }));
        assert_eq!(result.identifiers.get("task-a"), Some(&"ISSUE-A".into()));
    }

    #[test]
    fn fake_run_once_dispatches_runner_and_updates_snapshot_accounting() {
        let dir = fixture_dir("run-once");
        let workflow = write_workflow(&dir);

        let runtime = SymphonyRuntime::load(&workflow).expect("load runtime");
        let result = runtime.fake_run_once(1_000).expect("run once");

        assert!(result.tick.events.contains(&OrchestratorEvent::Dispatched {
            id: "task-a".into(),
            run_id: "symphony-dry-run-1".into(),
        }));
        let report = result.report.expect("agent runner report");
        assert!(matches!(
            report.result,
            crate::symphony::orchestrator::RunResult::Succeeded {
                input_tokens: 1,
                output_tokens: 1,
                turn_count: 1,
                ..
            }
        ));
        assert_eq!(result.snapshot.turn_count, 1);
        assert_eq!(result.snapshot.token_totals.input, 1);
        assert_eq!(result.snapshot.token_totals.output, 1);
        assert_eq!(result.snapshot.live_runtime_secs, 1);
        assert!(
            dir.join(".symphony")
                .join("workspaces")
                .join("ISSUE-A")
                .exists()
        );
    }
}
