//! Auto-commit Vulcan extension. Runs `git commit -am "vulcan
//! auto-commit"` against the **Session**'s `cwd` whenever the session
//! ends with uncommitted changes to tracked files. No-op when the
//! repo is clean or when `cwd` is not a git working tree.
//!
//! Daemon-side cargo-crate extension under GH issue #549. Self-
//! registers via `inventory::submit!` from this crate's compilation
//! unit; the daemon's startup path picks it up through
//! `vulcan::extensions::api::wire_inventory_into_registry`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use vulcan::extensions::api::{
    DaemonCodeExtension, ExtensionRegistration, SessionExtension, SessionExtensionCtx,
};
use vulcan::extensions::{ExtensionMetadata, ExtensionSource, ExtensionStatus};
use vulcan::hooks::HookHandler;
use vulcan_extension_macros::include_manifest;

/// Daemon-side factory. Holds no per-Session state; per-Session work
/// happens inside `AutoCommitSession`.
pub struct AutoCommitExtension;

impl Default for AutoCommitExtension {
    fn default() -> Self {
        Self
    }
}

impl DaemonCodeExtension for AutoCommitExtension {
    fn metadata(&self) -> ExtensionMetadata {
        let manifest = include_manifest!();
        let mut m = ExtensionMetadata::new(
            manifest.id,
            "Auto-Commit",
            manifest.version,
            ExtensionSource::Builtin,
        );
        m.status = ExtensionStatus::Active;
        m.requires_user_approval = manifest.requires_user_approval;
        m.description = "Runs `git commit -am` on session_end when there are uncommitted \
                         changes in the Session's cwd."
            .to_string();
        m
    }

    fn instantiate(&self, ctx: SessionExtensionCtx) -> Arc<dyn SessionExtension> {
        Arc::new(AutoCommitSession { cwd: ctx.cwd })
    }
}

/// Per-**Session** instance. Captures the session's `cwd` so the
/// hook handler knows which repo to commit against.
struct AutoCommitSession {
    cwd: PathBuf,
}

impl SessionExtension for AutoCommitSession {
    fn hook_handlers(&self) -> Vec<Arc<dyn HookHandler>> {
        vec![Arc::new(AutoCommitHook {
            cwd: self.cwd.clone(),
        })]
    }
}

struct AutoCommitHook {
    cwd: PathBuf,
}

#[async_trait]
impl HookHandler for AutoCommitHook {
    fn name(&self) -> &str {
        "auto-commit"
    }

    async fn session_end(&self, _session_id: &str, _total_turns: u32) {
        if !is_git_working_tree(&self.cwd).await {
            return;
        }
        if !has_uncommitted_tracked_changes(&self.cwd).await {
            return;
        }
        if let Err(err) = git_commit_all_tracked(&self.cwd, "vulcan auto-commit").await {
            tracing::warn!(cwd = %self.cwd.display(), %err, "auto-commit failed");
        }
    }
}

inventory::submit! {
    ExtensionRegistration {
        register: || Arc::new(AutoCommitExtension) as Arc<dyn DaemonCodeExtension>,
    }
}

async fn run_git(cwd: &Path, args: &[&str]) -> Result<std::process::Output> {
    Ok(tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .await?)
}

async fn is_git_working_tree(cwd: &Path) -> bool {
    match run_git(cwd, &["rev-parse", "--is-inside-work-tree"]).await {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

async fn has_uncommitted_tracked_changes(cwd: &Path) -> bool {
    // `git diff --quiet` exits 0 when working tree matches HEAD for
    // tracked files, 1 when there are differences. Cached changes
    // (already staged) need a second check via `--cached`.
    let working = run_git(cwd, &["diff", "--quiet"]).await;
    let staged = run_git(cwd, &["diff", "--cached", "--quiet"]).await;
    let dirty = match (working, staged) {
        (Ok(w), Ok(s)) => !w.status.success() || !s.status.success(),
        _ => false,
    };
    dirty
}

async fn git_commit_all_tracked(cwd: &Path, message: &str) -> Result<()> {
    let out = run_git(cwd, &["commit", "-am", message]).await?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("git commit failed: {}", stderr.trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    /// Bring up a tempdir as a git working tree with one initial
    /// commit so the suite can test against tracked-file modifications.
    fn init_repo() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().to_path_buf();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "vulcan-test@example.invalid"],
            vec!["config", "user.name", "Vulcan Test"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            let status = Command::new("git")
                .args(&args)
                .current_dir(&path)
                .status()
                .expect("git");
            assert!(status.success(), "git {args:?} failed");
        }
        std::fs::write(path.join("file.txt"), "v1\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init", "-q"])
            .current_dir(&path)
            .status()
            .unwrap();
        (dir, path)
    }

    fn commit_count(repo: &Path) -> usize {
        let out = Command::new("git")
            .args(["rev-list", "--count", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap();
        let s = String::from_utf8_lossy(&out.stdout);
        s.trim().parse().unwrap_or(0)
    }

    fn last_commit_subject(repo: &Path) -> String {
        let out = Command::new("git")
            .args(["log", "-1", "--pretty=%s"])
            .current_dir(repo)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[tokio::test]
    async fn session_end_creates_a_new_commit_when_repo_has_uncommitted_changes() {
        let (_guard, repo) = init_repo();
        std::fs::write(repo.join("file.txt"), "v2\n").unwrap();
        assert_eq!(commit_count(&repo), 1, "starts with one commit");

        let ext = AutoCommitExtension;
        let session = ext.instantiate(SessionExtensionCtx {
            cwd: repo.clone(),
            session_id: "test-session".to_string(),
            memory: Arc::new(vulcan::memory::SessionStore::in_memory()),
            frontend_capabilities: vulcan::extensions::FrontendCapability::full_set(),
            state: vulcan::extensions::ExtensionStateContext::in_memory_for_tests(
                "test-session",
                "auto-commit",
            ),
        });
        let handlers = session.hook_handlers();
        assert_eq!(handlers.len(), 1);
        handlers[0].session_end("test-session", 1).await;

        assert_eq!(commit_count(&repo), 2, "auto-commit added one commit");
        assert_eq!(last_commit_subject(&repo), "vulcan auto-commit");
    }

    #[tokio::test]
    async fn session_end_does_nothing_when_repo_is_clean() {
        let (_guard, repo) = init_repo();
        // No edits between init and session_end — repo stays clean.
        assert_eq!(commit_count(&repo), 1);

        let ext = AutoCommitExtension;
        let session = ext.instantiate(SessionExtensionCtx {
            cwd: repo.clone(),
            session_id: "clean-session".to_string(),
            memory: Arc::new(vulcan::memory::SessionStore::in_memory()),
            frontend_capabilities: vulcan::extensions::FrontendCapability::full_set(),
            state: vulcan::extensions::ExtensionStateContext::in_memory_for_tests(
                "clean-session",
                "auto-commit",
            ),
        });
        session.hook_handlers()[0]
            .session_end("clean-session", 0)
            .await;

        assert_eq!(commit_count(&repo), 1, "no new commit when clean");
        assert_eq!(last_commit_subject(&repo), "init");
    }

    #[tokio::test]
    async fn session_end_is_a_noop_outside_a_git_working_tree() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        // Not a git repo.

        let ext = AutoCommitExtension;
        let session = ext.instantiate(SessionExtensionCtx {
            cwd: path,
            session_id: "no-git".to_string(),
            memory: Arc::new(vulcan::memory::SessionStore::in_memory()),
            frontend_capabilities: vulcan::extensions::FrontendCapability::full_set(),
            state: vulcan::extensions::ExtensionStateContext::in_memory_for_tests(
                "no-git",
                "auto-commit",
            ),
        });
        session.hook_handlers()[0].session_end("no-git", 0).await;
        // No panic, no error — just silently no-op.
    }

    #[test]
    fn include_manifest_reads_package_metadata() {
        let manifest = include_manifest!();
        assert_eq!(manifest.id, "auto-commit");
        assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            manifest.daemon_entry.as_deref(),
            Some("vulcan_ext_auto_commit::AutoCommitExtension")
        );
        assert!(!manifest.requires_user_approval);
    }
}
