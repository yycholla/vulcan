//! Read-only Symphony task-source adapters and normalized fetch boundary.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde_json::Value as JsonValue;
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};
use thiserror::Error;

use crate::symphony::config::{TaskSourceConfig, TaskSourceKind};
use crate::symphony::workflow::NormalizedTask;

pub trait TaskSource: Send + Sync {
    fn capabilities(&self) -> TaskSourceCapabilities;
    fn fetch_candidates(
        &self,
        active_states: &[String],
    ) -> Result<Vec<NormalizedTask>, TaskSourceError>;
    fn fetch_by_state(&self, states: &[String]) -> Result<Vec<NormalizedTask>, TaskSourceError>;
    fn refresh_by_ids(&self, ids: &[String]) -> Result<Vec<NormalizedTask>, TaskSourceError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskSourceCapabilities {
    pub fetch_candidates: bool,
    pub fetch_by_state: bool,
    pub refresh_by_ids: bool,
}

#[derive(Debug, Clone)]
pub struct MarkdownTaskSource {
    path: PathBuf,
}

#[derive(Debug, Error)]
pub enum TaskSourceError {
    #[error("task source file `{path}` could not be read: {source}")]
    ReadFile {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("task source record {index} in `{path}` could not be parsed: {message}")]
    ParseRecord {
        path: String,
        index: usize,
        message: String,
    },

    #[error("task source record {index} in `{path}` is missing required field `{field}`")]
    MissingField {
        path: String,
        index: usize,
        field: &'static str,
    },

    #[error("task source kind `{kind}` is not supported by the read adapter factory")]
    UnsupportedKind { kind: String },
}

pub fn task_source_from_config(
    config: &TaskSourceConfig,
) -> Result<Box<dyn TaskSource>, TaskSourceError> {
    match &config.kind {
        TaskSourceKind::Markdown => {
            let markdown =
                config
                    .markdown
                    .as_ref()
                    .ok_or_else(|| TaskSourceError::UnsupportedKind {
                        kind: "markdown missing task_source.markdown".into(),
                    })?;
            Ok(Box::new(MarkdownTaskSource::new(markdown.path.clone())))
        }
        TaskSourceKind::Todo => {
            let todo = config
                .todo
                .as_ref()
                .ok_or_else(|| TaskSourceError::UnsupportedKind {
                    kind: "todo missing task_source.todo".into(),
                })?;
            Ok(Box::new(MarkdownTaskSource::new(todo.path.clone())))
        }
        other => Err(TaskSourceError::UnsupportedKind {
            kind: other.as_str().to_string(),
        }),
    }
}

impl MarkdownTaskSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn fetch_candidates(
        &self,
        active_states: &[String],
    ) -> Result<Vec<NormalizedTask>, TaskSourceError> {
        self.fetch_matching_states(active_states)
    }

    pub fn fetch_by_state(
        &self,
        states: &[String],
    ) -> Result<Vec<NormalizedTask>, TaskSourceError> {
        self.fetch_matching_states(states)
    }

    pub fn refresh_by_ids(&self, ids: &[String]) -> Result<Vec<NormalizedTask>, TaskSourceError> {
        let tasks = self.read_tasks()?;
        Ok(tasks
            .into_iter()
            .filter(|task| {
                ids.iter()
                    .any(|id| id == &task.id || id == &task.identifier)
            })
            .collect())
    }

    fn read_tasks(&self) -> Result<Vec<NormalizedTask>, TaskSourceError> {
        let raw = fs::read_to_string(&self.path).map_err(|source| TaskSourceError::ReadFile {
            path: self.path.display().to_string(),
            source,
        })?;
        parse_markdown_records(&raw, &self.path)
    }

    fn fetch_matching_states(
        &self,
        states: &[String],
    ) -> Result<Vec<NormalizedTask>, TaskSourceError> {
        let tasks = self.read_tasks()?;
        Ok(tasks
            .into_iter()
            .filter(|task| states.iter().any(|state| state == &task.state))
            .collect())
    }
}

impl TaskSource for MarkdownTaskSource {
    fn capabilities(&self) -> TaskSourceCapabilities {
        TaskSourceCapabilities {
            fetch_candidates: true,
            fetch_by_state: true,
            refresh_by_ids: true,
        }
    }

    fn fetch_candidates(
        &self,
        active_states: &[String],
    ) -> Result<Vec<NormalizedTask>, TaskSourceError> {
        MarkdownTaskSource::fetch_candidates(self, active_states)
    }

    fn fetch_by_state(&self, states: &[String]) -> Result<Vec<NormalizedTask>, TaskSourceError> {
        MarkdownTaskSource::fetch_by_state(self, states)
    }

    fn refresh_by_ids(&self, ids: &[String]) -> Result<Vec<NormalizedTask>, TaskSourceError> {
        MarkdownTaskSource::refresh_by_ids(self, ids)
    }
}

impl TaskSourceKind {
    fn as_str(&self) -> &str {
        match self {
            TaskSourceKind::GitHub => "github",
            TaskSourceKind::Markdown => "markdown",
            TaskSourceKind::Todo => "todo",
            TaskSourceKind::Other(value) => value,
        }
    }
}

fn parse_markdown_records(
    raw: &str,
    path: &std::path::Path,
) -> Result<Vec<NormalizedTask>, TaskSourceError> {
    let mut records = Vec::new();
    let mut current = Vec::new();
    let mut in_record = false;

    for line in raw.lines() {
        if line.trim() == "---" {
            if in_record {
                records.push(current.join("\n"));
                current.clear();
                in_record = false;
            } else {
                current.clear();
                in_record = true;
            }
        } else if in_record {
            current.push(line);
        }
    }

    records
        .iter()
        .enumerate()
        .map(|(idx, raw)| parse_record(raw, path, idx))
        .collect()
}

fn parse_record(
    raw: &str,
    path: &std::path::Path,
    index: usize,
) -> Result<NormalizedTask, TaskSourceError> {
    let value =
        serde_yaml::from_str::<YamlValue>(raw).map_err(|err| TaskSourceError::ParseRecord {
            path: path.display().to_string(),
            index,
            message: err.to_string(),
        })?;
    let YamlValue::Mapping(map) = value else {
        return Err(TaskSourceError::ParseRecord {
            path: path.display().to_string(),
            index,
            message: "record must be a map".into(),
        });
    };

    Ok(NormalizedTask {
        id: required_string(&map, path, index, "id")?,
        identifier: required_string(&map, path, index, "identifier")?,
        title: required_string(&map, path, index, "title")?,
        body: optional_string(&map, "body").unwrap_or_default(),
        priority: optional_string(&map, "priority"),
        state: required_string(&map, path, index, "state")?,
        branch: optional_string(&map, "branch"),
        labels: string_list(&map, "labels"),
        blockers: string_list(&map, "blockers"),
        url: optional_string(&map, "url"),
        path: optional_string(&map, "path"),
        created_at: optional_string(&map, "created_at"),
        updated_at: optional_string(&map, "updated_at"),
        source: source_map(&map),
    })
}

fn required_string(
    map: &YamlMapping,
    path: &std::path::Path,
    index: usize,
    field: &'static str,
) -> Result<String, TaskSourceError> {
    optional_string(map, field).ok_or_else(|| TaskSourceError::MissingField {
        path: path.display().to_string(),
        index,
        field,
    })
}

fn optional_string(map: &YamlMapping, field: &str) -> Option<String> {
    map.get(YamlValue::String(field.to_string()))
        .and_then(yaml_scalar_string)
}

fn string_list(map: &YamlMapping, field: &str) -> Vec<String> {
    let Some(YamlValue::Sequence(items)) = map.get(YamlValue::String(field.to_string())) else {
        return Vec::new();
    };
    items.iter().filter_map(yaml_scalar_string).collect()
}

fn source_map(map: &YamlMapping) -> BTreeMap<String, JsonValue> {
    let Some(YamlValue::Mapping(source)) = map.get(YamlValue::String("source".to_string())) else {
        return BTreeMap::new();
    };

    source
        .iter()
        .filter_map(|(key, value)| {
            let key = yaml_scalar_string(key)?;
            let value = serde_json::to_value(value).ok()?;
            Some((key, value))
        })
        .collect()
}

fn yaml_scalar_string(value: &YamlValue) -> Option<String> {
    match value {
        YamlValue::String(value) => Some(value.clone()),
        YamlValue::Number(value) => Some(value.to_string()),
        YamlValue::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_source_fetches_active_candidates_and_normalizes_payload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.md");
        fs::write(
            &path,
            r#"---
id: md-1
identifier: TASK-1
title: Build task source
body: Normalize records for Symphony.
state: ready-for-agent
priority: high
branch: yyc598-symphony-task-source
url: https://example.test/tasks/1
path: docs/tasks.md
labels: [Symphony, ready-for-agent]
blockers: [TASK-0]
created_at: 2026-05-01T10:00:00Z
updated_at: 2026-05-02T10:00:00Z
source:
  file: tasks.md
---

---
id: md-2
identifier: TASK-2
title: Done task
state: closed
---
"#,
        )
        .unwrap();

        let source = MarkdownTaskSource::new(path);
        let tasks = source
            .fetch_candidates(&["ready-for-agent".to_string()])
            .unwrap();

        assert_eq!(tasks.len(), 1);
        let task = &tasks[0];
        assert_eq!(task.id, "md-1");
        assert_eq!(task.identifier, "TASK-1");
        assert_eq!(task.title, "Build task source");
        assert_eq!(task.body, "Normalize records for Symphony.");
        assert_eq!(task.state, "ready-for-agent");
        assert_eq!(task.priority.as_deref(), Some("high"));
        assert_eq!(task.branch.as_deref(), Some("yyc598-symphony-task-source"));
        assert_eq!(task.path.as_deref(), Some("docs/tasks.md"));
        assert_eq!(task.url.as_deref(), Some("https://example.test/tasks/1"));
        assert_eq!(task.labels, ["Symphony", "ready-for-agent"]);
        assert_eq!(task.blockers, ["TASK-0"]);
        assert_eq!(task.created_at.as_deref(), Some("2026-05-01T10:00:00Z"));
        assert_eq!(task.updated_at.as_deref(), Some("2026-05-02T10:00:00Z"));
        assert_eq!(task.source["file"], serde_json::json!("tasks.md"));
    }

    #[test]
    fn markdown_source_fetches_by_state_and_refreshes_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.md");
        fs::write(
            &path,
            r#"---
id: md-1
identifier: TASK-1
title: Ready task
state: ready-for-agent
---

---
id: md-2
identifier: TASK-2
title: In review task
state: in-review
---

---
id: md-3
identifier: TASK-3
title: Closed task
state: closed
---
"#,
        )
        .unwrap();

        let source = MarkdownTaskSource::new(path);
        let by_state = source
            .fetch_by_state(&["in-review".to_string(), "closed".to_string()])
            .unwrap();
        assert_eq!(
            by_state
                .iter()
                .map(|task| task.identifier.as_str())
                .collect::<Vec<_>>(),
            ["TASK-2", "TASK-3"]
        );

        let refreshed = source
            .refresh_by_ids(&["md-3".to_string(), "TASK-1".to_string()])
            .unwrap();
        assert_eq!(
            refreshed
                .iter()
                .map(|task| (task.id.as_str(), task.state.as_str()))
                .collect::<Vec<_>>(),
            [("md-1", "ready-for-agent"), ("md-3", "closed")]
        );
    }

    #[test]
    fn task_source_factory_returns_abstraction_with_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.md");
        fs::write(
            &path,
            r#"---
id: md-1
identifier: TASK-1
title: Ready task
state: ready-for-agent
---
"#,
        )
        .unwrap();

        let config = crate::symphony::config::TaskSourceConfig {
            kind: crate::symphony::config::TaskSourceKind::Markdown,
            github: None,
            markdown: Some(crate::symphony::config::FileTaskSourceConfig { path }),
            todo: None,
        };

        let source = task_source_from_config(&config).unwrap();
        assert_eq!(
            source.capabilities(),
            TaskSourceCapabilities {
                fetch_candidates: true,
                fetch_by_state: true,
                refresh_by_ids: true,
            }
        );
        assert_eq!(
            source
                .fetch_candidates(&["ready-for-agent".to_string()])
                .unwrap()[0]
                .identifier,
            "TASK-1"
        );
    }

    #[test]
    fn markdown_source_reports_source_errors_and_missing_fields() {
        let missing = MarkdownTaskSource::new("/definitely/not/tasks.md");
        assert!(matches!(
            missing.fetch_candidates(&["ready-for-agent".to_string()]),
            Err(TaskSourceError::ReadFile { .. })
        ));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.md");
        fs::write(
            &path,
            r#"---
id: md-1
title: Missing identifier
state: ready-for-agent
---
"#,
        )
        .unwrap();

        let source = MarkdownTaskSource::new(path);
        assert!(matches!(
            source.fetch_candidates(&["ready-for-agent".to_string()]),
            Err(TaskSourceError::MissingField {
                field: "identifier",
                ..
            })
        ));
    }

    #[test]
    fn factory_reports_unsupported_source_kinds() {
        let config = crate::symphony::config::TaskSourceConfig {
            kind: crate::symphony::config::TaskSourceKind::GitHub,
            github: None,
            markdown: None,
            todo: None,
        };

        assert!(matches!(
            task_source_from_config(&config),
            Err(TaskSourceError::UnsupportedKind { kind }) if kind == "github"
        ));
    }

    #[test]
    fn factory_adapts_todo_file_sources_through_same_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("todos.md");
        fs::write(
            &path,
            r#"---
id: todo-1
identifier: TODO-1
title: Capture agent todo
state: ready-for-agent
priority: medium
path: .agents/todos.md
---
"#,
        )
        .unwrap();

        let config = crate::symphony::config::TaskSourceConfig {
            kind: crate::symphony::config::TaskSourceKind::Todo,
            github: None,
            markdown: None,
            todo: Some(crate::symphony::config::FileTaskSourceConfig { path }),
        };

        let source = task_source_from_config(&config).unwrap();
        let tasks = source
            .fetch_candidates(&["ready-for-agent".to_string()])
            .unwrap();

        assert_eq!(tasks[0].identifier, "TODO-1");
        assert_eq!(tasks[0].priority.as_deref(), Some("medium"));
        assert_eq!(tasks[0].path.as_deref(), Some(".agents/todos.md"));
    }
}
