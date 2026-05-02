//! Typed Symphony runtime config over workflow front matter.

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};

use serde_yaml::{Mapping as YamlMapping, Value as YamlValue};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveConfig {
    pub task_source: TaskSourceConfig,
    pub polling: PollingConfig,
    pub workspace: WorkspaceConfig,
    pub hooks: HooksConfig,
    pub agent: AgentConfig,
    pub codex: CodexConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSourceConfig {
    pub kind: TaskSourceKind,
    pub github: Option<GitHubTaskSourceConfig>,
    pub markdown: Option<FileTaskSourceConfig>,
    pub todo: Option<FileTaskSourceConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskSourceKind {
    GitHub,
    Markdown,
    Todo,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubTaskSourceConfig {
    pub repo: String,
    pub token: Option<String>,
    pub token_env: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTaskSourceConfig {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollingConfig {
    pub interval_secs: u64,
    pub active_states: Vec<String>,
    pub terminal_states: Vec<String>,
    pub max_concurrent: usize,
    pub state_concurrency: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceConfig {
    pub root: PathBuf,
    pub preserve_success: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HooksConfig {
    pub prepare: Vec<HookCommand>,
    pub cleanup: Vec<HookCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookCommand {
    pub command: String,
    pub timeout_secs: u64,
    pub fatal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentConfig {
    pub max_attempts: u32,
    pub max_turns: u32,
    pub stall_timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexConfig {
    pub command: String,
    pub args: Vec<String>,
    pub config: YamlMapping,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigView<'a> {
    root: &'a YamlMapping,
    repo_root: PathBuf,
    env: Option<&'a BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastKnownGoodConfig {
    current: EffectiveConfig,
    last_error: Option<ConfigError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadDecision {
    Applied(EffectiveConfig),
    KeptLastKnownGood {
        config: EffectiveConfig,
        error: ConfigError,
    },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ConfigError {
    #[error("missing required config `{path}`")]
    MissingRequired { path: String },

    #[error("invalid config `{path}`: {message}")]
    InvalidValue { path: String, message: String },

    #[error("environment variable `{var}` referenced by `{path}` is not set")]
    MissingEnv { path: String, var: String },

    #[error("environment variable `{var}` referenced by `{path}` is empty")]
    EmptyEnv { path: String, var: String },
}

impl<'a> ConfigView<'a> {
    pub fn new(root: &'a YamlMapping, repo_root: impl Into<PathBuf>) -> Self {
        Self {
            root,
            repo_root: repo_root.into(),
            env: None,
        }
    }

    pub fn with_env(
        root: &'a YamlMapping,
        repo_root: impl Into<PathBuf>,
        env: &'a BTreeMap<String, String>,
    ) -> Self {
        Self {
            root,
            repo_root: repo_root.into(),
            env: Some(env),
        }
    }

    pub fn effective_config(&self) -> Result<EffectiveConfig, ConfigError> {
        Ok(EffectiveConfig {
            task_source: self.task_source()?,
            polling: self.polling()?,
            workspace: self.workspace()?,
            hooks: self.hooks()?,
            agent: self.agent()?,
            codex: self.codex()?,
        })
    }

    pub fn task_source(&self) -> Result<TaskSourceConfig, ConfigError> {
        let map = self.required_map("task_source")?;
        let kind = self.required_string_in(map, "task_source.kind")?;
        let kind = TaskSourceKind::from(kind.as_str());
        let github = if matches!(kind, TaskSourceKind::GitHub) || map.get(key("github")).is_some() {
            Some(self.github_source(map)?)
        } else {
            None
        };
        let markdown = file_source(map, "markdown", &self.repo_root, self.env)?;
        let todo = file_source(map, "todo", &self.repo_root, self.env)?;

        match kind {
            TaskSourceKind::Markdown if markdown.is_none() => {
                return Err(missing("task_source.markdown.path"));
            }
            TaskSourceKind::Todo if todo.is_none() => return Err(missing("task_source.todo.path")),
            _ => {}
        }

        Ok(TaskSourceConfig {
            kind,
            github,
            markdown,
            todo,
        })
    }

    pub fn polling(&self) -> Result<PollingConfig, ConfigError> {
        let Some(map) = self.optional_map("polling")? else {
            return Ok(PollingConfig::default());
        };
        let mut config = PollingConfig::default();
        config.interval_secs =
            optional_u64(map, "polling.interval_secs")?.unwrap_or(config.interval_secs);
        config.active_states =
            optional_string_list(map, "polling.active_states")?.unwrap_or(config.active_states);
        config.terminal_states =
            optional_string_list(map, "polling.terminal_states")?.unwrap_or(config.terminal_states);
        config.max_concurrent =
            optional_usize(map, "polling.max_concurrent")?.unwrap_or(config.max_concurrent);
        if let Some(value) = map.get(key("state_concurrency")) {
            config.state_concurrency = parse_state_concurrency(value, "polling.state_concurrency")?;
        }
        Ok(config)
    }

    pub fn workspace(&self) -> Result<WorkspaceConfig, ConfigError> {
        let Some(map) = self.optional_map("workspace")? else {
            return Ok(WorkspaceConfig::default_for(&self.repo_root));
        };
        let mut config = WorkspaceConfig::default_for(&self.repo_root);
        if let Some(root) = optional_string(map, "workspace.root")? {
            config.root = self.normalize_path(&root);
        }
        config.preserve_success =
            optional_bool(map, "workspace.preserve_success")?.unwrap_or(config.preserve_success);
        Ok(config)
    }

    pub fn hooks(&self) -> Result<HooksConfig, ConfigError> {
        let Some(map) = self.optional_map("hooks")? else {
            return Ok(HooksConfig::default());
        };
        Ok(HooksConfig {
            prepare: optional_hooks(map, "prepare", "hooks.prepare")?,
            cleanup: optional_hooks(map, "cleanup", "hooks.cleanup")?,
        })
    }

    pub fn agent(&self) -> Result<AgentConfig, ConfigError> {
        let Some(map) = self.optional_map("agent")? else {
            return Ok(AgentConfig::default());
        };
        let mut config = AgentConfig::default();
        config.max_attempts =
            optional_u32(map, "agent.max_attempts")?.unwrap_or(config.max_attempts);
        config.max_turns = optional_u32(map, "agent.max_turns")?.unwrap_or(config.max_turns);
        config.stall_timeout_secs =
            optional_u64(map, "agent.stall_timeout_secs")?.or(config.stall_timeout_secs);
        Ok(config)
    }

    pub fn codex(&self) -> Result<CodexConfig, ConfigError> {
        let map = self.required_map("codex")?;
        let command = self.required_string_in(map, "codex.command")?;
        let args = optional_string_list(map, "codex.args")?.unwrap_or_default();
        let config = match map.get(key("config")) {
            Some(YamlValue::Mapping(map)) => map.clone(),
            Some(_) => {
                return Err(invalid("codex.config", "must be a map when provided"));
            }
            None => YamlMapping::new(),
        };
        Ok(CodexConfig {
            command,
            args,
            config,
        })
    }

    pub fn startup_validate(&self) -> Result<EffectiveConfig, ConfigError> {
        self.effective_config()
    }

    pub fn validate_for_dispatch(&self) -> Result<EffectiveConfig, ConfigError> {
        self.effective_config()
    }

    fn required_map(&self, path: &str) -> Result<&YamlMapping, ConfigError> {
        match self.root.get(key(path)) {
            Some(YamlValue::Mapping(map)) => Ok(map),
            Some(_) => Err(invalid(path, "must be a map")),
            None => Err(missing(path)),
        }
    }

    fn optional_map(&self, path: &str) -> Result<Option<&YamlMapping>, ConfigError> {
        match self.root.get(key(path)) {
            Some(YamlValue::Mapping(map)) => Ok(Some(map)),
            Some(_) => Err(invalid(path, "must be a map")),
            None => Ok(None),
        }
    }

    fn required_string_in(&self, map: &YamlMapping, path: &str) -> Result<String, ConfigError> {
        required_string(map, path)
    }

    fn github_source(
        &self,
        task_source: &YamlMapping,
    ) -> Result<GitHubTaskSourceConfig, ConfigError> {
        let Some(YamlValue::Mapping(map)) = task_source.get(key("github")) else {
            return Err(missing("task_source.github"));
        };
        let repo = required_string(map, "task_source.github.repo")?;
        let token_env = optional_string(map, "task_source.github.token_env")?;
        let token = if let Some(var) = &token_env {
            Some(self.resolve_env("task_source.github.token_env", var)?)
        } else {
            optional_string(map, "task_source.github.token")?
        };
        if token.as_ref().is_none_or(|value| value.is_empty()) {
            return Err(missing("task_source.github.token_env"));
        }
        Ok(GitHubTaskSourceConfig {
            repo,
            token,
            token_env,
        })
    }

    fn resolve_env(&self, path: &str, var: &str) -> Result<String, ConfigError> {
        let value = match self.env {
            Some(env) => env.get(var).cloned(),
            None => env::var(var).ok(),
        };
        match value {
            Some(value) if !value.is_empty() => Ok(value),
            Some(_) => Err(ConfigError::EmptyEnv {
                path: path.to_string(),
                var: var.to_string(),
            }),
            None => Err(ConfigError::MissingEnv {
                path: path.to_string(),
                var: var.to_string(),
            }),
        }
    }

    fn normalize_path(&self, raw: &str) -> PathBuf {
        normalize_path(raw, &self.repo_root, self.env)
    }
}

impl LastKnownGoodConfig {
    pub fn new(config: EffectiveConfig) -> Self {
        Self {
            current: config,
            last_error: None,
        }
    }

    pub fn current(&self) -> &EffectiveConfig {
        &self.current
    }

    pub fn last_error(&self) -> Option<&ConfigError> {
        self.last_error.as_ref()
    }

    pub fn apply_reload(&mut self, next: Result<EffectiveConfig, ConfigError>) -> ReloadDecision {
        match next {
            Ok(config) => {
                self.current = config.clone();
                self.last_error = None;
                ReloadDecision::Applied(config)
            }
            Err(error) => {
                self.last_error = Some(error.clone());
                ReloadDecision::KeptLastKnownGood {
                    config: self.current.clone(),
                    error,
                }
            }
        }
    }
}

impl Default for PollingConfig {
    fn default() -> Self {
        Self {
            interval_secs: 60,
            active_states: vec!["ready-for-agent".into()],
            terminal_states: vec!["closed".into(), "done".into(), "wontfix".into()],
            max_concurrent: 1,
            state_concurrency: BTreeMap::new(),
        }
    }
}

impl WorkspaceConfig {
    fn default_for(repo_root: &Path) -> Self {
        Self {
            root: repo_root.join(".symphony").join("workspaces"),
            preserve_success: true,
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            max_turns: 1,
            stall_timeout_secs: Some(900),
        }
    }
}

impl From<&str> for TaskSourceKind {
    fn from(value: &str) -> Self {
        match value {
            "github" => Self::GitHub,
            "markdown" => Self::Markdown,
            "todo" => Self::Todo,
            other => Self::Other(other.to_string()),
        }
    }
}

fn key(value: &str) -> YamlValue {
    YamlValue::String(value.to_string())
}

fn missing(path: &str) -> ConfigError {
    ConfigError::MissingRequired {
        path: path.to_string(),
    }
}

fn invalid(path: &str, message: &str) -> ConfigError {
    ConfigError::InvalidValue {
        path: path.to_string(),
        message: message.to_string(),
    }
}

fn file_source(
    task_source: &YamlMapping,
    key_name: &str,
    repo_root: &Path,
    env: Option<&BTreeMap<String, String>>,
) -> Result<Option<FileTaskSourceConfig>, ConfigError> {
    let Some(value) = task_source.get(key(key_name)) else {
        return Ok(None);
    };
    let YamlValue::Mapping(map) = value else {
        return Err(invalid(&format!("task_source.{key_name}"), "must be a map"));
    };
    let path = required_string(map, &format!("task_source.{key_name}.path"))?;
    Ok(Some(FileTaskSourceConfig {
        path: normalize_path(&path, repo_root, env),
    }))
}

fn required_string(map: &YamlMapping, path: &str) -> Result<String, ConfigError> {
    optional_string(map, path)?.ok_or_else(|| missing(path))
}

fn optional_string(map: &YamlMapping, path: &str) -> Result<Option<String>, ConfigError> {
    let leaf = leaf(path);
    match map.get(key(leaf)) {
        Some(YamlValue::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
        Some(YamlValue::String(_)) => Err(invalid(path, "must not be empty")),
        Some(value) => Err(invalid(
            path,
            &format!("must be a string, got {}", type_name(value)),
        )),
        None => Ok(None),
    }
}

fn optional_u64(map: &YamlMapping, path: &str) -> Result<Option<u64>, ConfigError> {
    let Some(value) = map.get(key(leaf(path))) else {
        return Ok(None);
    };
    parse_u64(value, path).map(Some)
}

fn optional_u32(map: &YamlMapping, path: &str) -> Result<Option<u32>, ConfigError> {
    let Some(value) = optional_u64(map, path)? else {
        return Ok(None);
    };
    u32::try_from(value)
        .map(Some)
        .map_err(|_| invalid(path, "is too large for u32"))
}

fn optional_usize(map: &YamlMapping, path: &str) -> Result<Option<usize>, ConfigError> {
    let Some(value) = optional_u64(map, path)? else {
        return Ok(None);
    };
    usize::try_from(value)
        .map(Some)
        .map_err(|_| invalid(path, "is too large for usize"))
}

fn parse_u64(value: &YamlValue, path: &str) -> Result<u64, ConfigError> {
    let parsed = match value {
        YamlValue::Number(number) => number.as_u64(),
        YamlValue::String(raw) => raw.parse::<u64>().ok(),
        _ => None,
    };
    parsed.ok_or_else(|| invalid(path, "must be a non-negative integer or numeric string"))
}

fn optional_bool(map: &YamlMapping, path: &str) -> Result<Option<bool>, ConfigError> {
    match map.get(key(leaf(path))) {
        Some(YamlValue::Bool(value)) => Ok(Some(*value)),
        Some(YamlValue::String(raw)) => match raw.as_str() {
            "true" => Ok(Some(true)),
            "false" => Ok(Some(false)),
            _ => Err(invalid(path, "must be a boolean")),
        },
        Some(_) => Err(invalid(path, "must be a boolean")),
        None => Ok(None),
    }
}

fn optional_string_list(map: &YamlMapping, path: &str) -> Result<Option<Vec<String>>, ConfigError> {
    let Some(value) = map.get(key(leaf(path))) else {
        return Ok(None);
    };
    let YamlValue::Sequence(items) = value else {
        return Err(invalid(path, "must be a list of strings"));
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item {
            YamlValue::String(value) if !value.trim().is_empty() => out.push(value.clone()),
            YamlValue::String(_) => return Err(invalid(path, "list items must not be empty")),
            _ => return Err(invalid(path, "list items must be strings")),
        }
    }
    Ok(Some(out))
}

fn parse_state_concurrency(
    value: &YamlValue,
    path: &str,
) -> Result<BTreeMap<String, usize>, ConfigError> {
    let YamlValue::Mapping(map) = value else {
        return Err(invalid(path, "must be a map"));
    };
    let mut out = BTreeMap::new();
    for (state, limit) in map {
        let YamlValue::String(state) = state else {
            return Err(invalid(path, "state names must be strings"));
        };
        let value = parse_u64(limit, &format!("{path}.{state}"))?;
        let value = usize::try_from(value)
            .map_err(|_| invalid(&format!("{path}.{state}"), "is too large for usize"))?;
        out.insert(state.clone(), value);
    }
    Ok(out)
}

fn optional_hooks(
    map: &YamlMapping,
    key_name: &str,
    path: &str,
) -> Result<Vec<HookCommand>, ConfigError> {
    let Some(value) = map.get(key(key_name)) else {
        return Ok(Vec::new());
    };
    let YamlValue::Sequence(items) = value else {
        return Err(invalid(path, "must be a list"));
    };
    let mut out = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        let item_path = format!("{path}[{idx}]");
        let YamlValue::Mapping(map) = item else {
            return Err(invalid(&item_path, "must be a map"));
        };
        out.push(HookCommand {
            command: required_string(map, &format!("{item_path}.command"))?,
            timeout_secs: optional_u64(map, &format!("{item_path}.timeout_secs"))?.unwrap_or(300),
            fatal: optional_bool(map, &format!("{item_path}.fatal"))?
                .unwrap_or(key_name == "prepare"),
        });
    }
    Ok(out)
}

fn normalize_path(raw: &str, repo_root: &Path, env: Option<&BTreeMap<String, String>>) -> PathBuf {
    let expanded = if raw == "~" {
        home_dir(env).unwrap_or_else(|| PathBuf::from(raw))
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home_dir(env)
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(raw))
    } else {
        PathBuf::from(raw)
    };
    if expanded.is_absolute() {
        expanded
    } else {
        repo_root.join(expanded)
    }
}

fn home_dir(env: Option<&BTreeMap<String, String>>) -> Option<PathBuf> {
    if let Some(env) = env {
        let home = env.get("HOME")?;
        if home.is_empty() {
            return None;
        }
        return Some(PathBuf::from(home));
    }
    let home = env::var_os("HOME")?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home))
}

fn leaf(path: &str) -> &str {
    path.rsplit_once('.').map_or(path, |(_, leaf)| leaf)
}

fn type_name(value: &YamlValue) -> &'static str {
    match value {
        YamlValue::Null => "null",
        YamlValue::Bool(_) => "boolean",
        YamlValue::Number(_) => "number",
        YamlValue::String(_) => "string",
        YamlValue::Sequence(_) => "sequence",
        YamlValue::Mapping(_) => "map",
        YamlValue::Tagged(_) => "tagged value",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(raw: &str) -> YamlMapping {
        match serde_yaml::from_str::<YamlValue>(raw).unwrap() {
            YamlValue::Mapping(map) => map,
            other => panic!("expected map, got {other:?}"),
        }
    }

    fn repo_root() -> PathBuf {
        PathBuf::from("/repo")
    }

    fn env_map(values: &[(&str, &str)]) -> BTreeMap<String, String> {
        values
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn typed_getters_cover_runtime_sections() {
        let env = env_map(&[("SYMPHONY_GITHUB_TOKEN", "secret")]);
        let root = map(r#"
task_source:
  kind: github
  github:
    repo: yycholla/vulcan
    token_env: SYMPHONY_GITHUB_TOKEN
polling:
  interval_secs: "15"
  active_states: [ready-for-agent, ready-for-human]
  terminal_states: [closed]
  max_concurrent: "2"
  state_concurrency:
    ready-for-human: "1"
workspace:
  root: workspaces
  preserve_success: false
hooks:
  prepare:
    - command: ./bootstrap.sh
      timeout_secs: "45"
  cleanup:
    - command: ./cleanup.sh
      fatal: false
agent:
  max_attempts: "4"
  max_turns: "3"
  stall_timeout_secs: "120"
codex:
  command: codex
  args: ["--profile", "symphony"]
  config:
    model: gpt-5.3-codex
"#);

        let config = ConfigView::with_env(&root, repo_root(), &env)
            .effective_config()
            .unwrap();

        assert_eq!(config.task_source.kind, TaskSourceKind::GitHub);
        assert_eq!(config.task_source.github.unwrap().repo, "yycholla/vulcan");
        assert_eq!(config.polling.interval_secs, 15);
        assert_eq!(
            config.polling.active_states,
            ["ready-for-agent", "ready-for-human"]
        );
        assert_eq!(
            config.polling.state_concurrency.get("ready-for-human"),
            Some(&1)
        );
        assert_eq!(config.workspace.root, PathBuf::from("/repo/workspaces"));
        assert!(!config.workspace.preserve_success);
        assert_eq!(config.hooks.prepare[0].timeout_secs, 45);
        assert!(!config.hooks.cleanup[0].fatal);
        assert_eq!(config.agent.max_attempts, 4);
        assert_eq!(config.agent.max_turns, 3);
        assert_eq!(config.agent.stall_timeout_secs, Some(120));
        assert_eq!(config.codex.command, "codex");
        assert_eq!(config.codex.args, ["--profile", "symphony"]);
        assert!(config.codex.config.contains_key(key("model")));
    }

    #[test]
    fn defaults_fill_optional_sections() {
        let env = env_map(&[("SYMPHONY_GITHUB_TOKEN", "secret")]);
        let root = map(r#"
task_source:
  kind: github
  github:
    repo: yycholla/vulcan
    token_env: SYMPHONY_GITHUB_TOKEN
codex:
  command: codex
"#);

        let config = ConfigView::with_env(&root, repo_root(), &env)
            .effective_config()
            .unwrap();

        assert_eq!(config.polling, PollingConfig::default());
        assert_eq!(
            config.workspace,
            WorkspaceConfig::default_for(Path::new("/repo"))
        );
        assert_eq!(config.hooks, HooksConfig::default());
        assert_eq!(config.agent, AgentConfig::default());
    }

    #[test]
    fn empty_env_values_are_rejected() {
        let env = env_map(&[("SYMPHONY_EMPTY_TOKEN", "")]);
        let root = map(r#"
task_source:
  kind: github
  github:
    repo: yycholla/vulcan
    token_env: SYMPHONY_EMPTY_TOKEN
codex:
  command: codex
"#);

        let err = ConfigView::with_env(&root, repo_root(), &env)
            .effective_config()
            .unwrap_err();
        assert!(matches!(err, ConfigError::EmptyEnv { .. }));
    }

    #[test]
    fn path_expansion_handles_home_and_relative_paths() {
        let env = env_map(&[("HOME", "/home/tester")]);
        let root = map(r#"
task_source:
  kind: markdown
  markdown:
    path: tasks.md
workspace:
  root: ~/symphony-work
codex:
  command: codex
"#);

        let config = ConfigView::with_env(&root, repo_root(), &env)
            .effective_config()
            .unwrap();

        assert_eq!(
            config.task_source.markdown.unwrap().path,
            PathBuf::from("/repo/tasks.md")
        );
        assert_eq!(
            config.workspace.root,
            PathBuf::from("/home/tester/symphony-work")
        );
    }

    #[test]
    fn startup_validation_requires_task_source_kind() {
        let root = map(r#"
task_source:
  github:
    repo: yycholla/vulcan
codex:
  command: codex
"#);

        let err = ConfigView::new(&root, repo_root())
            .startup_validate()
            .unwrap_err();
        assert_eq!(err, missing("task_source.kind"));
    }

    #[test]
    fn startup_validation_requires_source_specific_config() {
        let root = map(r#"
task_source:
  kind: markdown
codex:
  command: codex
"#);

        let err = ConfigView::new(&root, repo_root())
            .startup_validate()
            .unwrap_err();
        assert_eq!(err, missing("task_source.markdown.path"));
    }

    #[test]
    fn startup_validation_requires_source_auth_when_required() {
        let env = env_map(&[]);
        let root = map(r#"
task_source:
  kind: github
  github:
    repo: yycholla/vulcan
    token_env: SYMPHONY_MISSING_TOKEN
codex:
  command: codex
"#);

        let err = ConfigView::with_env(&root, repo_root(), &env)
            .startup_validate()
            .unwrap_err();
        assert!(matches!(err, ConfigError::MissingEnv { .. }));
    }

    #[test]
    fn startup_validation_requires_codex_command() {
        let root = map(r#"
task_source:
  kind: markdown
  markdown:
    path: tasks.md
codex: {}
"#);

        let err = ConfigView::new(&root, repo_root())
            .startup_validate()
            .unwrap_err();
        assert_eq!(err, missing("codex.command"));
    }

    #[test]
    fn per_tick_validation_returns_error_without_mutating_last_good() {
        let good = map(r#"
task_source:
  kind: markdown
  markdown:
    path: tasks.md
codex:
  command: codex
"#);
        let bad = map(r#"
task_source:
  kind: markdown
  markdown:
    path: tasks.md
codex: {}
"#);
        let mut lkg = LastKnownGoodConfig::new(
            ConfigView::new(&good, repo_root())
                .startup_validate()
                .unwrap(),
        );

        let decision = lkg.apply_reload(ConfigView::new(&bad, repo_root()).validate_for_dispatch());

        let ReloadDecision::KeptLastKnownGood { config, error } = decision else {
            panic!("bad reload should keep last good config");
        };
        assert_eq!(error, missing("codex.command"));
        assert_eq!(config.codex.command, "codex");
        assert_eq!(lkg.current().codex.command, "codex");
        assert_eq!(lkg.last_error(), Some(&missing("codex.command")));
    }

    #[test]
    fn valid_reload_replaces_last_known_good_and_clears_error() {
        let first = map(r#"
task_source:
  kind: markdown
  markdown:
    path: tasks.md
polling:
  interval_secs: 60
codex:
  command: codex
"#);
        let second = map(r#"
task_source:
  kind: markdown
  markdown:
    path: tasks.md
polling:
  interval_secs: "5"
codex:
  command: codex-next
"#);
        let mut lkg = LastKnownGoodConfig::new(
            ConfigView::new(&first, repo_root())
                .startup_validate()
                .unwrap(),
        );

        let decision =
            lkg.apply_reload(ConfigView::new(&second, repo_root()).validate_for_dispatch());

        let ReloadDecision::Applied(config) = decision else {
            panic!("valid reload should apply");
        };
        assert_eq!(config.polling.interval_secs, 5);
        assert_eq!(lkg.current().codex.command, "codex-next");
        assert_eq!(lkg.last_error(), None);
    }

    #[test]
    fn invalid_numeric_string_is_rejected() {
        let root = map(r#"
task_source:
  kind: markdown
  markdown:
    path: tasks.md
polling:
  interval_secs: soon
codex:
  command: codex
"#);

        let err = ConfigView::new(&root, repo_root())
            .effective_config()
            .unwrap_err();
        assert!(
            matches!(err, ConfigError::InvalidValue { path, .. } if path == "polling.interval_secs")
        );
    }
}
