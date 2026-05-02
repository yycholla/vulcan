//! Repository-owned `WORKFLOW.md` loading and strict prompt rendering.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct Workflow {
    pub config: YamlMapping,
    pub prompt_template: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedTask {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub priority: Option<String>,
    pub state: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub source: BTreeMap<String, JsonValue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptInput {
    pub issue: NormalizedTask,
    #[serde(default)]
    pub attempt: Option<u32>,
}

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("missing workflow file: {path}")]
    MissingWorkflowFile { path: String },

    #[error("workflow parse error: {0}")]
    WorkflowParseError(String),

    #[error("workflow front matter must be a map")]
    WorkflowFrontMatterNotAMap,

    #[error("template parse error: {0}")]
    TemplateParseError(String),

    #[error("template render error: {0}")]
    TemplateRenderError(String),
}

impl Workflow {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, WorkflowError> {
        let path = path.as_ref();
        let raw = fs::read_to_string(path).map_err(|err| WorkflowError::MissingWorkflowFile {
            path: format!("{} ({err})", path.display()),
        })?;
        Self::parse(&raw)
    }

    pub fn parse(raw: &str) -> Result<Self, WorkflowError> {
        let (front_matter, body) = split_front_matter(raw)?;
        let config = match front_matter {
            Some(raw_yaml) if raw_yaml.trim().is_empty() => YamlMapping::new(),
            Some(raw_yaml) => match serde_yaml::from_str::<YamlValue>(raw_yaml) {
                Ok(YamlValue::Mapping(map)) => map,
                Ok(_) => return Err(WorkflowError::WorkflowFrontMatterNotAMap),
                Err(err) => return Err(WorkflowError::WorkflowParseError(err.to_string())),
            },
            None => YamlMapping::new(),
        };

        Ok(Self {
            config,
            prompt_template: body.trim().to_string(),
        })
    }

    pub fn render_prompt(&self, input: &PromptInput) -> Result<String, WorkflowError> {
        render_template(&self.prompt_template, &input.to_context()?)
    }
}

impl PromptInput {
    fn to_context(&self) -> Result<JsonValue, WorkflowError> {
        serde_json::to_value(self)
            .map_err(|err| WorkflowError::TemplateRenderError(err.to_string()))
    }
}

fn split_front_matter(raw: &str) -> Result<(Option<&str>, &str), WorkflowError> {
    let Some(rest) = raw.strip_prefix("---") else {
        return Ok((None, raw));
    };
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))
        .ok_or_else(|| {
            WorkflowError::WorkflowParseError(
                "front matter opening marker must be followed by a newline".into(),
            )
        })?;

    for marker in ["\n---\n", "\n---\r\n", "\r\n---\r\n", "\r\n---\n"] {
        if let Some(idx) = rest.find(marker) {
            let yaml = &rest[..idx];
            let body = &rest[idx + marker.len()..];
            return Ok((Some(yaml), body));
        }
    }

    Err(WorkflowError::WorkflowParseError(
        "front matter opening marker has no closing marker".into(),
    ))
}

fn render_template(template: &str, context: &JsonValue) -> Result<String, WorkflowError> {
    render_section(template, context, &BTreeMap::new())
}

fn render_section(
    template: &str,
    context: &JsonValue,
    locals: &BTreeMap<String, JsonValue>,
) -> Result<String, WorkflowError> {
    let mut out = String::new();
    let mut cursor = 0;

    while let Some(rel_start) = template[cursor..].find('{') {
        let start = cursor + rel_start;
        out.push_str(&template[cursor..start]);
        if template[start..].starts_with("{{") {
            let close = template[start + 2..]
                .find("}}")
                .ok_or_else(|| WorkflowError::TemplateParseError("unclosed variable tag".into()))?
                + start
                + 2;
            let expr = template[start + 2..close].trim();
            out.push_str(&render_expr(expr, context, locals)?);
            cursor = close + 2;
        } else if template[start..].starts_with("{%") {
            let close = template[start + 2..]
                .find("%}")
                .ok_or_else(|| WorkflowError::TemplateParseError("unclosed control tag".into()))?
                + start
                + 2;
            let tag = template[start + 2..close].trim();
            if let Some(for_tag) = tag.strip_prefix("for ") {
                let (local_name, path) = parse_for_tag(for_tag)?;
                let body_start = close + 2;
                let (body, after_loop) = find_loop_body(template, body_start)?;
                let values = resolve_path(path, context, locals)?;
                let array = values.as_array().ok_or_else(|| {
                    WorkflowError::TemplateRenderError(format!("`{path}` is not iterable"))
                })?;
                for item in array {
                    let mut loop_locals = locals.clone();
                    loop_locals.insert(local_name.to_string(), item.clone());
                    out.push_str(&render_section(body, context, &loop_locals)?);
                }
                cursor = after_loop;
            } else if tag == "endfor" {
                return Err(WorkflowError::TemplateParseError(
                    "unexpected endfor tag".into(),
                ));
            } else {
                return Err(WorkflowError::TemplateParseError(format!(
                    "unsupported control tag `{tag}`"
                )));
            }
        } else {
            out.push('{');
            cursor = start + 1;
        }
    }

    out.push_str(&template[cursor..]);
    Ok(out)
}

fn render_expr(
    expr: &str,
    context: &JsonValue,
    locals: &BTreeMap<String, JsonValue>,
) -> Result<String, WorkflowError> {
    if let Some((_, filter)) = expr.split_once('|') {
        let name = filter
            .split(':')
            .next()
            .unwrap_or(filter)
            .split_whitespace()
            .next()
            .unwrap_or(filter.trim());
        return Err(WorkflowError::TemplateRenderError(format!(
            "unknown filter `{name}`"
        )));
    }
    let value = resolve_path(expr, context, locals)?;
    Ok(display_value(value))
}

fn parse_for_tag(raw: &str) -> Result<(&str, &str), WorkflowError> {
    let mut parts = raw.split_whitespace();
    let local = parts
        .next()
        .ok_or_else(|| WorkflowError::TemplateParseError("for tag missing local".into()))?;
    let in_keyword = parts
        .next()
        .ok_or_else(|| WorkflowError::TemplateParseError("for tag missing `in`".into()))?;
    let path = parts
        .next()
        .ok_or_else(|| WorkflowError::TemplateParseError("for tag missing iterable".into()))?;
    if in_keyword != "in" || parts.next().is_some() {
        return Err(WorkflowError::TemplateParseError(
            "for tag must be `for item in collection`".into(),
        ));
    }
    Ok((local, path))
}

fn find_loop_body(template: &str, body_start: usize) -> Result<(&str, usize), WorkflowError> {
    let mut cursor = body_start;
    let mut depth = 1usize;

    while let Some(rel_start) = template[cursor..].find("{%") {
        let tag_start = cursor + rel_start;
        let tag_close = template[tag_start + 2..]
            .find("%}")
            .ok_or_else(|| WorkflowError::TemplateParseError("unclosed control tag".into()))?
            + tag_start
            + 2;
        let tag = template[tag_start + 2..tag_close].trim();
        if tag.starts_with("for ") {
            depth += 1;
        } else if tag == "endfor" {
            depth -= 1;
            if depth == 0 {
                return Ok((&template[body_start..tag_start], tag_close + 2));
            }
        }
        cursor = tag_close + 2;
    }

    Err(WorkflowError::TemplateParseError(
        "for tag has no matching endfor".into(),
    ))
}

fn resolve_path<'a>(
    path: &str,
    context: &'a JsonValue,
    locals: &'a BTreeMap<String, JsonValue>,
) -> Result<&'a JsonValue, WorkflowError> {
    if path.trim().is_empty() {
        return Err(WorkflowError::TemplateRenderError(
            "empty variable path".into(),
        ));
    }

    let mut segments = path.split('.');
    let first = segments.next().unwrap();
    let mut current = if let Some(local) = locals.get(first) {
        local
    } else {
        context.get(first).ok_or_else(|| {
            WorkflowError::TemplateRenderError(format!("unknown variable `{first}`"))
        })?
    };

    for segment in segments {
        current = current.get(segment).ok_or_else(|| {
            WorkflowError::TemplateRenderError(format!("unknown variable `{path}`"))
        })?;
    }

    Ok(current)
}

fn display_value(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => String::new(),
        JsonValue::Bool(v) => v.to_string(),
        JsonValue::Number(v) => v.to_string(),
        JsonValue::String(v) => v.clone(),
        JsonValue::Array(_) | JsonValue::Object(_) => value.to_string(),
    }
}

impl fmt::Display for NormalizedTask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.identifier, self.title)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map as JsonMap;

    fn task() -> NormalizedTask {
        let mut source = BTreeMap::new();
        source.insert(
            "url".to_string(),
            JsonValue::String("https://github.com/yycholla/vulcan/issues/595".to_string()),
        );
        NormalizedTask {
            id: "595".to_string(),
            identifier: "GH-595".to_string(),
            title: "Workflow loader".to_string(),
            body: "Build the workflow contract.".to_string(),
            priority: None,
            state: "ready-for-agent".to_string(),
            branch: None,
            labels: vec!["Feature".to_string(), "ready-for-agent".to_string()],
            blockers: vec!["GH-594".to_string()],
            url: Some("https://github.com/yycholla/vulcan/issues/595".to_string()),
            path: None,
            created_at: None,
            updated_at: None,
            source,
        }
    }

    #[test]
    fn parses_workflow_without_front_matter() {
        let workflow = Workflow::parse("  Use the issue.\n").unwrap();
        assert!(workflow.config.is_empty());
        assert_eq!(workflow.prompt_template, "Use the issue.");
    }

    #[test]
    fn parses_workflow_with_map_front_matter_and_unknown_keys() {
        let workflow = Workflow::parse(
            r#"---
tracker:
  kind: github
server:
  port: 9910
---

Run {{ issue.title }}
"#,
        )
        .unwrap();

        assert!(
            workflow
                .config
                .contains_key(YamlValue::String("tracker".into()))
        );
        assert!(
            workflow
                .config
                .contains_key(YamlValue::String("server".into()))
        );
        assert_eq!(workflow.prompt_template, "Run {{ issue.title }}");
    }

    #[test]
    fn rejects_malformed_front_matter() {
        let err = Workflow::parse("---\ntracker: [\n---\nbody").unwrap_err();
        assert!(matches!(err, WorkflowError::WorkflowParseError(_)));
    }

    #[test]
    fn rejects_non_map_front_matter() {
        let err = Workflow::parse("---\n- tracker\n---\nbody").unwrap_err();
        assert!(matches!(err, WorkflowError::WorkflowFrontMatterNotAMap));
    }

    #[test]
    fn rejects_unclosed_front_matter() {
        let err = Workflow::parse("---\ntracker:\n  kind: github\nbody").unwrap_err();
        assert!(matches!(err, WorkflowError::WorkflowParseError(_)));
    }

    #[test]
    fn renders_task_fields_attempt_labels_blockers_and_source_refs() {
        let workflow = Workflow::parse(
            r#"Issue {{ issue.identifier }}: {{ issue.title }}
State: {{ issue.state }}
Attempt: {{ attempt }}
Labels:{% for label in issue.labels %} {{ label }}{% endfor %}
Blockers:{% for blocker in issue.blockers %} {{ blocker }}{% endfor %}
Source: {{ issue.source.url }}"#,
        )
        .unwrap();
        let rendered = workflow
            .render_prompt(&PromptInput {
                issue: task(),
                attempt: Some(2),
            })
            .unwrap();

        assert!(rendered.contains("Issue GH-595: Workflow loader"));
        assert!(rendered.contains("State: ready-for-agent"));
        assert!(rendered.contains("Attempt: 2"));
        assert!(rendered.contains("Labels: Feature ready-for-agent"));
        assert!(rendered.contains("Blockers: GH-594"));
        assert!(rendered.contains("Source: https://github.com/yycholla/vulcan/issues/595"));
    }

    #[test]
    fn renders_absent_attempt_as_empty() {
        let workflow = Workflow::parse("Attempt={{ attempt }}").unwrap();
        let rendered = workflow
            .render_prompt(&PromptInput {
                issue: task(),
                attempt: None,
            })
            .unwrap();
        assert_eq!(rendered, "Attempt=");
    }

    #[test]
    fn rejects_unknown_variable() {
        let workflow = Workflow::parse("{{ issue.missing }}").unwrap();
        let err = workflow
            .render_prompt(&PromptInput {
                issue: task(),
                attempt: None,
            })
            .unwrap_err();
        assert!(matches!(err, WorkflowError::TemplateRenderError(_)));
    }

    #[test]
    fn rejects_unknown_filter() {
        let workflow = Workflow::parse("{{ issue.title | upcase }}").unwrap();
        let err = workflow
            .render_prompt(&PromptInput {
                issue: task(),
                attempt: None,
            })
            .unwrap_err();
        assert!(matches!(err, WorkflowError::TemplateRenderError(_)));
    }

    #[test]
    fn rejects_template_parse_failures() {
        let workflow = Workflow::parse("{% for label in issue.labels %}{{ label }}").unwrap();
        let err = workflow
            .render_prompt(&PromptInput {
                issue: task(),
                attempt: None,
            })
            .unwrap_err();
        assert!(matches!(err, WorkflowError::TemplateParseError(_)));
    }

    #[test]
    fn missing_workflow_file_is_distinct() {
        let err = Workflow::load("definitely-not-a-real-workflow-file.md").unwrap_err();
        assert!(matches!(err, WorkflowError::MissingWorkflowFile { .. }));
    }

    #[test]
    fn nested_loops_render() {
        let mut item = JsonMap::new();
        item.insert(
            "children".to_string(),
            JsonValue::Array(vec![
                JsonValue::String("a".into()),
                JsonValue::String("b".into()),
            ]),
        );
        let mut source = BTreeMap::new();
        source.insert(
            "items".to_string(),
            JsonValue::Array(vec![JsonValue::Object(item)]),
        );
        let issue = NormalizedTask { source, ..task() };
        let workflow = Workflow::parse(
            "{% for item in issue.source.items %}{% for child in item.children %}{{ child }}{% endfor %}{% endfor %}",
        )
        .unwrap();

        let rendered = workflow
            .render_prompt(&PromptInput {
                issue,
                attempt: None,
            })
            .unwrap();
        assert_eq!(rendered, "ab");
    }
}
