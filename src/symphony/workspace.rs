//! Per-task Symphony workspace preparation and lifecycle hooks.

use std::fs;
use std::path::{Component, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::symphony::config::{HooksConfig, WorkspaceConfig};
use crate::symphony::workflow::NormalizedTask;

#[derive(Debug, Clone)]
pub struct WorkspaceManager {
    config: WorkspaceConfig,
    hooks: WorkspaceHooks,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedWorkspace {
    pub key: String,
    pub path: PathBuf,
    pub created_now: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceHooks {
    pub after_create: Option<String>,
    pub before_run: Option<String>,
    pub after_run: Option<String>,
    pub before_remove: Option<String>,
    pub timeout: Duration,
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("workspace path `{path}` escapes root `{root}`")]
    EscapesRoot { path: String, root: String },

    #[error("workspace path `{path}` is not a directory")]
    NotDirectory { path: String },

    #[error("workspace filesystem error for `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("workspace hook `{hook}` failed in `{cwd}` with status {status}: {stderr}")]
    HookFailed {
        hook: &'static str,
        cwd: String,
        status: String,
        stderr: String,
    },

    #[error("workspace hook `{hook}` timed out after {timeout_ms}ms in `{cwd}`")]
    HookTimedOut {
        hook: &'static str,
        cwd: String,
        timeout_ms: u128,
    },
}

impl WorkspaceManager {
    pub fn new(config: WorkspaceConfig) -> Self {
        Self::with_hooks(config, WorkspaceHooks::default())
    }

    pub fn with_hooks(config: WorkspaceConfig, hooks: WorkspaceHooks) -> Self {
        Self { config, hooks }
    }

    pub fn from_config(config: WorkspaceConfig, hooks: HooksConfig) -> Self {
        Self::with_hooks(config, hooks.into())
    }

    pub fn prepare(&self, task: &NormalizedTask) -> Result<PreparedWorkspace, WorkspaceError> {
        let key = workspace_key(&task.identifier);
        let path = contained_workspace_path(&self.config.root, &key)?;
        let created_now = create_workspace_dir(&path)?;
        if created_now {
            self.run_fatal_hook("after_create", self.hooks.after_create.as_deref(), &path)?;
        }

        Ok(PreparedWorkspace {
            key,
            path,
            created_now,
        })
    }

    pub fn before_run(&self, workspace: &PreparedWorkspace) -> Result<(), WorkspaceError> {
        self.run_fatal_hook(
            "before_run",
            self.hooks.before_run.as_deref(),
            &workspace.path,
        )
    }

    pub fn after_run(&self, workspace: &PreparedWorkspace) {
        self.run_best_effort_hook(
            "after_run",
            self.hooks.after_run.as_deref(),
            &workspace.path,
        );
    }

    pub fn remove(&self, workspace: &PreparedWorkspace) -> Result<(), WorkspaceError> {
        self.run_best_effort_hook(
            "before_remove",
            self.hooks.before_remove.as_deref(),
            &workspace.path,
        );
        if !workspace.path.exists() {
            return Ok(());
        }
        fs::remove_dir_all(&workspace.path).map_err(|source| WorkspaceError::Io {
            path: workspace.path.display().to_string(),
            source,
        })
    }

    fn run_fatal_hook(
        &self,
        name: &'static str,
        script: Option<&str>,
        cwd: &std::path::Path,
    ) -> Result<(), WorkspaceError> {
        let Some(script) = script else {
            return Ok(());
        };
        let mut child = Command::new("sh")
            .arg("-lc")
            .arg(script)
            .current_dir(cwd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|source| WorkspaceError::Io {
                path: cwd.display().to_string(),
                source,
            })?;
        let deadline = Instant::now() + self.hooks.timeout;
        while Instant::now() < deadline {
            if child
                .try_wait()
                .map_err(|source| WorkspaceError::Io {
                    path: cwd.display().to_string(),
                    source,
                })?
                .is_some()
            {
                let output = child
                    .wait_with_output()
                    .map_err(|source| WorkspaceError::Io {
                        path: cwd.display().to_string(),
                        source,
                    })?;
                return hook_output_result(name, cwd, output);
            }
            thread::sleep(Duration::from_millis(5));
        }

        let _ = child.kill();
        let _ = child.wait();
        Err(WorkspaceError::HookTimedOut {
            hook: name,
            cwd: cwd.display().to_string(),
            timeout_ms: self.hooks.timeout.as_millis(),
        })
    }

    fn run_best_effort_hook(
        &self,
        name: &'static str,
        script: Option<&str>,
        cwd: &std::path::Path,
    ) {
        if let Err(err) = self.run_fatal_hook(name, script, cwd) {
            tracing::warn!(hook = name, cwd = %cwd.display(), error = %err, "workspace hook failed");
        }
    }
}

fn hook_output_result(
    name: &'static str,
    cwd: &std::path::Path,
    output: std::process::Output,
) -> Result<(), WorkspaceError> {
    if output.status.success() {
        return Ok(());
    }
    Err(WorkspaceError::HookFailed {
        hook: name,
        cwd: cwd.display().to_string(),
        status: output
            .status
            .code()
            .map_or_else(|| "signal".to_string(), |code| code.to_string()),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

impl Default for WorkspaceHooks {
    fn default() -> Self {
        Self {
            after_create: None,
            before_run: None,
            after_run: None,
            before_remove: None,
            timeout: Duration::from_secs(60),
        }
    }
}

impl From<HooksConfig> for WorkspaceHooks {
    fn from(config: HooksConfig) -> Self {
        Self {
            after_create: config.after_create,
            before_run: config.before_run,
            after_run: config.after_run,
            before_remove: config.before_remove,
            timeout: Duration::from_millis(config.timeout_ms),
        }
    }
}

fn workspace_key(identifier: &str) -> String {
    identifier
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn contained_workspace_path(root: &std::path::Path, key: &str) -> Result<PathBuf, WorkspaceError> {
    let root = normalize_lexically(root);
    let path = root.join(key);
    if path.starts_with(&root) {
        Ok(path)
    } else {
        Err(WorkspaceError::EscapesRoot {
            path: path.display().to_string(),
            root: root.display().to_string(),
        })
    }
}

fn create_workspace_dir(path: &std::path::Path) -> Result<bool, WorkspaceError> {
    if path.exists() {
        if path.is_dir() {
            return Ok(false);
        }
        return Err(WorkspaceError::NotDirectory {
            path: path.display().to_string(),
        });
    }

    fs::create_dir_all(path).map_err(|source| WorkspaceError::Io {
        path: path.display().to_string(),
        source,
    })?;
    Ok(true)
}

fn normalize_lexically(path: &std::path::Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(_) | Component::RootDir | Component::Prefix(_) => {
                out.push(component.as_os_str());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::TempDir;

    use super::*;

    fn task(identifier: &str) -> NormalizedTask {
        NormalizedTask {
            id: "task-1".into(),
            identifier: identifier.into(),
            title: "Implement the thing".into(),
            body: "Body".into(),
            state: "ready-for-agent".into(),
            labels: Vec::new(),
            blockers: Vec::new(),
            url: None,
            source: Default::default(),
        }
    }

    #[test]
    fn prepare_creates_sanitized_workspace_and_reports_reuse() {
        let temp = TempDir::new().unwrap();
        let config = WorkspaceConfig {
            root: temp.path().join("root"),
            preserve_success: true,
        };
        let manager = WorkspaceManager::new(config);

        let first = manager.prepare(&task("GH/597: workspace manager")).unwrap();

        assert_eq!(first.key, "GH_597__workspace_manager");
        assert_eq!(
            first.path,
            temp.path().join("root/GH_597__workspace_manager")
        );
        assert!(first.created_now);
        assert!(Path::new(&first.path).is_dir());

        let second = manager.prepare(&task("GH/597: workspace manager")).unwrap();

        assert_eq!(second.key, first.key);
        assert_eq!(second.path, first.path);
        assert!(!second.created_now);
    }

    #[test]
    fn prepare_normalizes_root_before_containment_check() {
        let temp = TempDir::new().unwrap();
        let config = WorkspaceConfig {
            root: temp.path().join("outer/../root"),
            preserve_success: true,
        };
        let manager = WorkspaceManager::new(config);

        let prepared = manager.prepare(&task("../../escape")).unwrap();

        assert_eq!(prepared.key, ".._.._escape");
        assert_eq!(prepared.path, temp.path().join("root/.._.._escape"));
        assert!(prepared.path.starts_with(temp.path().join("root")));
        assert!(prepared.path.is_dir());
    }

    #[test]
    fn after_create_hook_runs_only_for_new_workspace_with_workspace_cwd() {
        let temp = TempDir::new().unwrap();
        let config = WorkspaceConfig {
            root: temp.path().join("root"),
            preserve_success: true,
        };
        let hooks = WorkspaceHooks {
            after_create: Some("pwd > hook.cwd".into()),
            ..WorkspaceHooks::default()
        };
        let manager = WorkspaceManager::with_hooks(config, hooks);
        let task = task("GH-597");

        let prepared = manager.prepare(&task).unwrap();
        let cwd = std::fs::read_to_string(prepared.path.join("hook.cwd")).unwrap();
        assert_eq!(cwd.trim(), prepared.path.display().to_string());

        std::fs::remove_file(prepared.path.join("hook.cwd")).unwrap();
        let reused = manager.prepare(&task).unwrap();
        assert!(!reused.created_now);
        assert!(!reused.path.join("hook.cwd").exists());
    }

    #[test]
    fn after_create_failure_is_fatal() {
        let temp = TempDir::new().unwrap();
        let config = WorkspaceConfig {
            root: temp.path().join("root"),
            preserve_success: true,
        };
        let hooks = WorkspaceHooks {
            after_create: Some("echo failed >&2; exit 6".into()),
            ..WorkspaceHooks::default()
        };
        let manager = WorkspaceManager::with_hooks(config, hooks);

        let err = manager.prepare(&task("GH-597")).unwrap_err();

        assert!(matches!(
            err,
            WorkspaceError::HookFailed {
                hook: "after_create",
                ..
            }
        ));
    }

    #[test]
    fn before_run_is_fatal_but_after_run_is_best_effort() {
        let temp = TempDir::new().unwrap();
        let config = WorkspaceConfig {
            root: temp.path().join("root"),
            preserve_success: true,
        };
        let hooks = WorkspaceHooks {
            before_run: Some("echo before >&2; exit 7".into()),
            after_run: Some("echo after >&2; exit 9".into()),
            ..WorkspaceHooks::default()
        };
        let manager = WorkspaceManager::with_hooks(config, hooks);
        let prepared = manager.prepare(&task("GH-597")).unwrap();

        let err = manager.before_run(&prepared).unwrap_err();
        assert!(matches!(
            err,
            WorkspaceError::HookFailed {
                hook: "before_run",
                ..
            }
        ));

        manager.after_run(&prepared);
    }

    #[test]
    fn before_remove_is_best_effort_and_workspace_is_removed() {
        let temp = TempDir::new().unwrap();
        let config = WorkspaceConfig {
            root: temp.path().join("root"),
            preserve_success: true,
        };
        let hooks = WorkspaceHooks {
            before_remove: Some("echo cleanup failed >&2; exit 8".into()),
            ..WorkspaceHooks::default()
        };
        let manager = WorkspaceManager::with_hooks(config, hooks);
        let prepared = manager.prepare(&task("GH-597")).unwrap();

        manager.remove(&prepared).unwrap();

        assert!(!prepared.path.exists());
    }

    #[test]
    fn fatal_hooks_time_out() {
        let temp = TempDir::new().unwrap();
        let config = WorkspaceConfig {
            root: temp.path().join("root"),
            preserve_success: true,
        };
        let hooks = WorkspaceHooks {
            before_run: Some("sleep 1".into()),
            timeout: Duration::from_millis(20),
            ..WorkspaceHooks::default()
        };
        let manager = WorkspaceManager::with_hooks(config, hooks);
        let prepared = manager.prepare(&task("GH-597")).unwrap();

        let err = manager.before_run(&prepared).unwrap_err();

        assert!(matches!(
            err,
            WorkspaceError::HookTimedOut {
                hook: "before_run",
                ..
            }
        ));
    }

    #[test]
    fn manager_uses_hooks_from_typed_config() {
        let temp = TempDir::new().unwrap();
        let config = WorkspaceConfig {
            root: temp.path().join("root"),
            preserve_success: true,
        };
        let hooks = crate::symphony::config::HooksConfig {
            after_create: Some("echo created > marker".into()),
            before_run: None,
            after_run: None,
            before_remove: None,
            timeout_ms: 250,
        };
        let manager = WorkspaceManager::from_config(config, hooks);

        let prepared = manager.prepare(&task("GH-597")).unwrap();

        assert_eq!(
            std::fs::read_to_string(prepared.path.join("marker")).unwrap(),
            "created\n"
        );
    }
}
