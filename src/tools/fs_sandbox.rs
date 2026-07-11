//! YYC-248: filesystem sandbox for the file tools.
//!
//! `read_file`, `write_file`, `edit_file`, `search_files`, and
//! `list_files` accept arbitrary absolute paths today. With prompt
//! injection that is enough to read `/etc/shadow`, `~/.ssh/id_rsa`,
//! `/proc/self/environ`, or write a fresh entry into `~/.bashrc` for
//! persistence.
//!
//! This module enforces a **default-deny prefix list** that covers the
//! best-known sensitive paths on Linux/macOS. Paths are canonicalized
//! (or, if the target doesn't exist yet, the parent is canonicalized)
//! before the prefix check so a symlink at `cwd/escape -> /etc/passwd`
//! cannot defeat it.
//!
//! A session-local [`FsSandbox`] also roots relative paths in the active
//! workspace and contains restricted, sensitive, and untrusted workspaces.
//! Trusted workspaces retain access to non-sensitive paths outside the root.

use crate::trust::TrustLevel;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FsSandboxError {
    #[error("blocked path `{path}`: this prefix is in the default-deny list ({reason})")]
    BlockedPrefix { path: String, reason: &'static str },
    #[error("path `{path}` could not be resolved: {source}")]
    Resolve {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("blocked path `{path}`: outside workspace `{workspace_root}` for {trust_level} trust")]
    OutsideWorkspace {
        path: String,
        workspace_root: String,
        trust_level: &'static str,
    },
}

#[derive(Debug, Clone)]
pub struct FsSandbox {
    workspace_root: PathBuf,
    trust_level: TrustLevel,
}

impl FsSandbox {
    pub fn new(workspace_root: impl AsRef<Path>, trust_level: TrustLevel) -> Self {
        let workspace_root = workspace_root.as_ref();
        let workspace_root =
            std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
        Self {
            workspace_root,
            trust_level,
        }
    }

    pub fn validate_read(&self, raw: impl AsRef<Path>) -> Result<PathBuf, FsSandboxError> {
        self.validate(raw)
    }

    pub fn validate_write(&self, raw: impl AsRef<Path>) -> Result<PathBuf, FsSandboxError> {
        self.validate(raw)
    }

    fn validate(&self, raw: impl AsRef<Path>) -> Result<PathBuf, FsSandboxError> {
        let raw = raw.as_ref();
        let resolved = resolve_path(raw, &self.workspace_root)?;
        let display = raw.to_string_lossy();
        check_denylist(&resolved, &display)?;
        if self.trust_level != TrustLevel::Trusted && !resolved.starts_with(&self.workspace_root) {
            return Err(FsSandboxError::OutsideWorkspace {
                path: display.into_owned(),
                workspace_root: self.workspace_root.display().to_string(),
                trust_level: self.trust_level.as_str(),
            });
        }
        Ok(resolved)
    }
}

/// Backward-compatible denylist validation for callers without a session
/// policy. Agent-built file tools apply their explicit policy first.
pub fn validate_read(raw: &str) -> Result<PathBuf, FsSandboxError> {
    FsSandbox::new(current_dir(), TrustLevel::Trusted).validate_read(raw)
}

pub fn validate_write(raw: &str) -> Result<PathBuf, FsSandboxError> {
    FsSandbox::new(current_dir(), TrustLevel::Trusted).validate_write(raw)
}

fn resolve_path(raw: &Path, workspace_root: &Path) -> Result<PathBuf, FsSandboxError> {
    let absolute = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        workspace_root.join(raw)
    };
    let mut cursor = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match std::fs::canonicalize(cursor) {
            Ok(mut resolved) => {
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = cursor.file_name() else {
                    return Err(FsSandboxError::Resolve {
                        path: raw.display().to_string(),
                        source: error,
                    });
                };
                missing.push(name.to_os_string());
                let Some(parent) = cursor.parent() else {
                    return Err(FsSandboxError::Resolve {
                        path: raw.display().to_string(),
                        source: error,
                    });
                };
                cursor = parent;
            }
            Err(source) => {
                return Err(FsSandboxError::Resolve {
                    path: raw.display().to_string(),
                    source,
                });
            }
        }
    }
}

fn current_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn check_denylist(canonical: &Path, raw: &str) -> Result<(), FsSandboxError> {
    if let Some(reason) = classify_blocked(canonical) {
        return Err(FsSandboxError::BlockedPrefix {
            path: raw.to_string(),
            reason,
        });
    }
    Ok(())
}

fn classify_blocked(path: &Path) -> Option<&'static str> {
    let s = path.to_string_lossy();

    // Pseudo-filesystems.
    if starts_with_any(&s, &["/proc/", "/proc"]) {
        return Some("/proc — process pseudo-fs (env vars, fds, /proc/self leaks)");
    }
    if starts_with_any(&s, &["/sys/", "/sys"]) {
        return Some("/sys — kernel sysfs");
    }
    // /dev — allow the well-known generic sinks; refuse the rest.
    if s == "/dev" || s.starts_with("/dev/") {
        const SAFE_DEV: &[&str] = &[
            "/dev/null",
            "/dev/zero",
            "/dev/random",
            "/dev/urandom",
            "/dev/stdin",
            "/dev/stdout",
            "/dev/stderr",
            "/dev/tty",
        ];
        if !SAFE_DEV.contains(&s.as_ref()) {
            return Some("/dev — raw device files");
        }
    }

    // System credential / config files.
    if s == "/etc/shadow"
        || s == "/etc/gshadow"
        || s.starts_with("/etc/shadow-")
        || s.starts_with("/etc/sudoers")
    {
        return Some("system credential / sudo config");
    }

    // User credential / state directories. Block read AND write — `~/.aws`
    // contains AWS keys, `~/.kube/config` has cluster admin tokens, etc.
    if let Some(home) = home_dir() {
        const HOME_DENY: &[&str] = &[
            ".ssh",
            ".gnupg",
            ".aws",
            ".kube",
            ".docker",
            ".vulcan",
            ".config/gh",
            ".config/anthropic",
            ".config/op", // 1Password CLI
            ".config/gcloud",
            ".azure",
            ".pgpass",
            ".netrc",
        ];
        for entry in HOME_DENY {
            let target = home.join(entry);
            if path == target.as_path() || path.starts_with(&target) {
                return Some("user credential / agent-state directory");
            }
        }
    }

    None
}

fn home_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    if home.is_empty() {
        return None;
    }
    Some(PathBuf::from(home))
}

fn starts_with_any(s: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| s == *p || s.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::TrustLevel;
    use tempfile::tempdir;

    #[test]
    fn contained_trust_levels_reject_outside_paths_and_symlinks() {
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("outside.txt");
        std::fs::write(&outside_file, "secret").unwrap();
        let escape = workspace.path().join("escape");
        std::os::unix::fs::symlink(outside.path(), &escape).unwrap();
        let traversal = PathBuf::from("..")
            .join(outside.path().file_name().unwrap())
            .join("outside.txt");

        for level in [
            TrustLevel::Restricted,
            TrustLevel::Sensitive,
            TrustLevel::Untrusted,
        ] {
            let sandbox = FsSandbox::new(workspace.path(), level);
            for result in [
                sandbox.validate_read(&outside_file),
                sandbox.validate_write(outside.path().join("new.txt")),
                sandbox.validate_read("escape/outside.txt"),
                sandbox.validate_write("escape/new.txt"),
                sandbox.validate_read(&traversal),
            ] {
                let error = result.unwrap_err();
                assert!(matches!(error, FsSandboxError::OutsideWorkspace { .. }));
                assert!(error.to_string().contains("outside workspace"));
            }
        }
    }

    #[test]
    fn trusted_workspace_allows_non_sensitive_outside_paths() {
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("outside.txt");
        std::fs::write(&outside_file, "allowed").unwrap();
        let sandbox = FsSandbox::new(workspace.path(), TrustLevel::Trusted);

        assert_eq!(sandbox.validate_read(&outside_file).unwrap(), outside_file);
        assert!(
            sandbox
                .validate_write(outside.path().join("new.txt"))
                .is_ok()
        );
        assert!(matches!(
            sandbox.validate_read("/etc/shadow").unwrap_err(),
            FsSandboxError::BlockedPrefix { .. }
        ));
    }

    #[test]
    fn relative_paths_resolve_against_the_explicit_workspace_root() {
        let workspace = tempdir().unwrap();
        let inside = workspace.path().join("inside.txt");
        std::fs::write(&inside, "inside").unwrap();
        let sandbox = FsSandbox::new(workspace.path(), TrustLevel::Untrusted);

        assert_eq!(sandbox.validate_read("inside.txt").unwrap(), inside);
        assert_eq!(
            sandbox.validate_write("nested/new.txt").unwrap(),
            workspace.path().join("nested/new.txt")
        );
    }

    #[test]
    fn blocks_read_of_etc_shadow() {
        let err = validate_read("/etc/shadow").unwrap_err();
        assert!(matches!(err, FsSandboxError::BlockedPrefix { .. }));
    }

    #[test]
    fn blocks_read_of_proc_self_environ() {
        let err = validate_read("/proc/self/environ").unwrap_err();
        match err {
            FsSandboxError::BlockedPrefix { reason, .. } => assert!(reason.starts_with("/proc")),
            _ => panic!("expected BlockedPrefix"),
        }
    }

    #[test]
    fn blocks_read_of_sys() {
        let err = validate_read("/sys/kernel/notes").unwrap_err();
        assert!(matches!(err, FsSandboxError::BlockedPrefix { .. }));
    }

    #[test]
    fn blocks_read_of_dev_block_devices() {
        for raw in ["/dev/sda", "/dev/nvme0n1", "/dev/disk0"] {
            let err = validate_read(raw).unwrap_err();
            assert!(
                matches!(err, FsSandboxError::BlockedPrefix { .. }),
                "expected block for {raw}"
            );
        }
    }

    #[test]
    fn allows_read_of_safe_dev_paths() {
        // /dev/null / /dev/urandom always exist on Linux/macOS.
        validate_read("/dev/null").unwrap();
        validate_read("/dev/urandom").unwrap();
    }

    #[test]
    fn blocks_read_of_ssh_key_paths() {
        if let Some(home) = home_dir() {
            let bogus = home.join(".ssh").join("nope_does_not_exist");
            let err = validate_read(bogus.to_str().unwrap()).unwrap_err();
            assert!(matches!(err, FsSandboxError::BlockedPrefix { .. }));
        }
    }

    #[test]
    fn blocks_read_of_aws_credentials() {
        if let Some(home) = home_dir() {
            let bogus = home.join(".aws").join("credentials");
            let err = validate_read(bogus.to_str().unwrap()).unwrap_err();
            assert!(matches!(err, FsSandboxError::BlockedPrefix { .. }));
        }
    }

    #[test]
    fn blocks_read_of_vulcan_state() {
        if let Some(home) = home_dir() {
            let bogus = home.join(".vulcan").join("config.toml");
            let err = validate_read(bogus.to_str().unwrap()).unwrap_err();
            assert!(matches!(err, FsSandboxError::BlockedPrefix { .. }));
        }
    }

    #[test]
    fn blocks_write_to_etc_sudoers() {
        let err = validate_write("/etc/sudoers.d/00-evil").unwrap_err();
        assert!(matches!(err, FsSandboxError::BlockedPrefix { .. }));
    }

    #[test]
    fn allows_write_to_temp_dir() {
        let tmp =
            std::env::temp_dir().join(format!("vulcan-fs-sandbox-test-{}.txt", std::process::id()));
        let p = validate_write(tmp.to_str().unwrap()).unwrap();
        assert!(p.is_absolute());
    }

    #[test]
    fn blocks_symlink_escape_via_canonicalize() {
        // tempdir + symlink → /etc → canonicalize lands on /etc → not in
        // the deny list. But a symlink to /etc/shadow is. Build that.
        let tmp =
            std::env::temp_dir().join(format!("vulcan-fs-sandbox-symlink-{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        // Skip on platforms that disallow symlinking to /etc/shadow.
        if std::os::unix::fs::symlink("/etc/shadow", &tmp).is_err() {
            return;
        }
        let err = validate_read(tmp.to_str().unwrap()).unwrap_err();
        let _ = std::fs::remove_file(&tmp);
        assert!(matches!(err, FsSandboxError::BlockedPrefix { .. }));
    }

    #[test]
    fn allows_read_of_unrelated_file_under_tempdir() {
        let tmp = std::env::temp_dir().join(format!(
            "vulcan-fs-sandbox-allow-{}.txt",
            std::process::id()
        ));
        std::fs::write(&tmp, b"hello").unwrap();
        let result = validate_read(tmp.to_str().unwrap());
        let _ = std::fs::remove_file(&tmp);
        result.unwrap();
    }

    #[test]
    fn read_of_nonexistent_unblocked_path_succeeds() {
        // Sandbox returns the absolute path; the file tool surfaces
        // the NotFound itself.
        let p = validate_read("/tmp/vulcan-sandbox-no-such-file-xyz123").unwrap();
        assert!(p.is_absolute());
    }
}
