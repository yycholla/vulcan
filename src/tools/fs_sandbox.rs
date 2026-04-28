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
//! Workspace-root sandboxing (only allow paths under `cwd` unless the
//! trust profile permits otherwise) is deliberately deferred to a
//! follow-up — too disruptive for ops/dev workflows that legitimately
//! read outside the project. The deny prefix list catches the worst
//! exfiltration vectors without requiring a trust-profile rewrite.

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
}

/// Validate a path the tool wants to *read*. Returns the canonicalized
/// path on success.
pub fn validate_read(raw: &str) -> Result<PathBuf, FsSandboxError> {
    let resolved = resolve_for_read(raw)?;
    check_denylist(&resolved, raw)?;
    Ok(resolved)
}

/// Validate a path the tool wants to *write* (create or overwrite).
/// Returns the canonicalized path on success.
pub fn validate_write(raw: &str) -> Result<PathBuf, FsSandboxError> {
    let resolved = resolve_for_write(raw)?;
    check_denylist(&resolved, raw)?;
    Ok(resolved)
}

fn resolve_for_read(raw: &str) -> Result<PathBuf, FsSandboxError> {
    let absolute = absolutize(raw);
    match std::fs::canonicalize(&absolute) {
        Ok(p) => Ok(p),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Reads of non-existent paths are not the sandbox's
            // problem — let the file tool surface its own NotFound.
            // Still return the absolute path so the prefix check fires
            // first (so a missing file under a blocked prefix doesn't
            // leak the prefix as a NotFound error).
            Ok(absolute)
        }
        Err(source) => Err(FsSandboxError::Resolve {
            path: raw.to_string(),
            source,
        }),
    }
}

fn resolve_for_write(raw: &str) -> Result<PathBuf, FsSandboxError> {
    let absolute = absolutize(raw);
    if let Ok(p) = std::fs::canonicalize(&absolute) {
        return Ok(p);
    }
    // Target doesn't exist yet. Canonicalize the parent, then re-attach
    // the filename. This is the standard symlink-defeating idiom for
    // "where will my new file actually land?"
    let parent = absolute.parent().ok_or_else(|| FsSandboxError::Resolve {
        path: raw.to_string(),
        source: std::io::Error::other("no parent directory"),
    })?;
    let canon_parent = std::fs::canonicalize(parent).map_err(|source| FsSandboxError::Resolve {
        path: raw.to_string(),
        source,
    })?;
    let filename = absolute
        .file_name()
        .ok_or_else(|| FsSandboxError::Resolve {
            path: raw.to_string(),
            source: std::io::Error::other("path has no filename component"),
        })?;
    Ok(canon_parent.join(filename))
}

fn absolutize(raw: &str) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        return p;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    cwd.join(p)
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
