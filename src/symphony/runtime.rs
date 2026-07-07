//! Runtime facade for Symphony CLI and future service callers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::symphony::config::{ConfigView, EffectiveConfig};
use crate::symphony::orchestrator::{
    Orchestrator, OrchestratorEvent, RunnerStart, RunnerStartResult, TaskRunner,
};
use crate::symphony::task_source::task_source_from_config;
use crate::symphony::workflow::{NormalizedTask, Workflow};

#[derive(Debug, Clone)]
pub struct SymphonyRuntime {
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
        Ok(Self { config })
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
}
