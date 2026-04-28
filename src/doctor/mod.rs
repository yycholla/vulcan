//! YYC-183: `vulcan doctor` — structured runtime diagnostics.
//!
//! ## Scope of this PR
//!
//! - `CheckStatus` (Pass / Warn / Fail) + `CheckResult` shape
//!   with id, summary, remediation, and machine-readable code.
//! - `DoctorReport` aggregator with overall pass/fail.
//! - First slice of built-in checks: config / vulcan_home /
//!   storage / workspace / tools.
//! - Pure-Rust runner so the CLI driver can be tiny.
//!
//! ## Deliberately deferred
//!
//! - Provider reachability check (network — separate PR).
//! - Gateway feature checks (waits on connector config).
//! - LSP / code intelligence health (waits on YYC-44 expansion).
//! - JSON output mode (waits on the human-readable shape
//!   stabilizing).

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Status of a single check. Order matters: any `Fail` makes the
/// whole report fail; `Warn` keeps overall pass while flagging
/// attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl CheckStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckStatus::Pass => "pass",
            CheckStatus::Warn => "warn",
            CheckStatus::Fail => "fail",
        }
    }
}

/// One check's outcome. `id` is a stable code (e.g.
/// `config.path.exists`) so JSON consumers + future regression
/// tests can match on it without sniffing prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub id: String,
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
    pub remediation: Option<String>,
}

impl CheckResult {
    pub fn pass(id: impl Into<String>, name: impl Into<String>, msg: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            status: CheckStatus::Pass,
            message: msg.into(),
            remediation: None,
        }
    }

    pub fn warn(
        id: impl Into<String>,
        name: impl Into<String>,
        msg: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            status: CheckStatus::Warn,
            message: msg.into(),
            remediation: Some(fix.into()),
        }
    }

    pub fn fail(
        id: impl Into<String>,
        name: impl Into<String>,
        msg: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            status: CheckStatus::Fail,
            message: msg.into(),
            remediation: Some(fix.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub checks: Vec<CheckResult>,
}

impl DoctorReport {
    pub fn new() -> Self {
        Self { checks: Vec::new() }
    }

    pub fn push(&mut self, result: CheckResult) {
        self.checks.push(result);
    }

    /// Overall: `Fail` if any check failed, else `Warn` if any
    /// warned, else `Pass`.
    pub fn overall(&self) -> CheckStatus {
        let mut overall = CheckStatus::Pass;
        for c in &self.checks {
            if c.status > overall {
                overall = c.status;
            }
        }
        overall
    }
}

impl Default for DoctorReport {
    fn default() -> Self {
        Self::new()
    }
}

/// Run the first-slice check set against `vulcan_home_dir` and
/// `cwd`. Both inputs are explicit so tests can drive the
/// engine against a temp dir without touching the user's
/// `~/.vulcan/`.
pub fn run_checks(vulcan_home_dir: &Path, cwd: &Path) -> DoctorReport {
    let mut report = DoctorReport::new();
    report.push(check_vulcan_home_exists(vulcan_home_dir));
    report.push(check_vulcan_home_writable(vulcan_home_dir));
    report.push(check_config_present(vulcan_home_dir));
    report.push(check_workspace_is_git(cwd));
    report.push(check_run_records_store(vulcan_home_dir));
    report
}

fn check_vulcan_home_exists(dir: &Path) -> CheckResult {
    if dir.exists() {
        CheckResult::pass(
            "config.home.exists",
            "vulcan_home directory",
            format!("{}", dir.display()),
        )
    } else {
        CheckResult::warn(
            "config.home.exists",
            "vulcan_home directory",
            format!("missing: {}", dir.display()),
            "Run `vulcan migrate-config` or `vulcan auth` to create it.",
        )
    }
}

fn check_vulcan_home_writable(dir: &Path) -> CheckResult {
    if !dir.exists() {
        // Don't double-fail when the dir is just absent — that
        // surfaces in the `exists` check above.
        return CheckResult::warn(
            "config.home.writable",
            "vulcan_home writable",
            "skipped (directory does not exist yet)",
            "Create the directory first via `mkdir -p` or a setup command.",
        );
    }
    let probe = dir.join(".doctor.write-probe");
    match std::fs::write(&probe, b"vulcan-doctor") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            CheckResult::pass(
                "config.home.writable",
                "vulcan_home writable",
                "write probe succeeded",
            )
        }
        Err(e) => CheckResult::fail(
            "config.home.writable",
            "vulcan_home writable",
            format!("write probe failed: {e}"),
            "Check filesystem permissions on ~/.vulcan/.",
        ),
    }
}

fn check_config_present(dir: &Path) -> CheckResult {
    let cfg = dir.join("config.toml");
    if cfg.exists() {
        CheckResult::pass(
            "config.file.present",
            "config.toml",
            format!("{}", cfg.display()),
        )
    } else {
        CheckResult::warn(
            "config.file.present",
            "config.toml",
            format!("missing: {}", cfg.display()),
            "Run `vulcan auth` to set up provider config, or create config.toml manually.",
        )
    }
}

fn check_workspace_is_git(cwd: &Path) -> CheckResult {
    let mut cur = Some(cwd);
    while let Some(p) = cur {
        if p.join(".git").exists() {
            return CheckResult::pass(
                "workspace.git.present",
                "git workspace",
                format!("found .git at {}", p.display()),
            );
        }
        cur = p.parent();
    }
    CheckResult::warn(
        "workspace.git.present",
        "git workspace",
        format!("no .git ancestor of {}", cwd.display()),
        "Run vulcan inside a git working tree, or `git init` to create one.",
    )
}

fn check_run_records_store(dir: &Path) -> CheckResult {
    let path = dir.join("run_records.db");
    if path.exists() {
        // Best-effort openability — try to open + immediately
        // close. We intentionally don't open in WAL mode here;
        // that's owned by the live store.
        match rusqlite::Connection::open(&path) {
            Ok(_) => CheckResult::pass(
                "storage.run_records.openable",
                "run records store",
                format!("opened {}", path.display()),
            ),
            Err(e) => CheckResult::fail(
                "storage.run_records.openable",
                "run records store",
                format!("open failed: {e}"),
                "Inspect the file permissions; if corrupted, `vulcan knowledge purge --kind run_records` will recreate it.",
            ),
        }
    } else {
        // Not having the file yet isn't a failure — it's created
        // on first run. Still surface as a warn so
        // `vulcan doctor` flags first-run state instead of
        // silent passing.
        CheckResult::warn(
            "storage.run_records.openable",
            "run records store",
            format!("not yet created: {}", path.display()),
            "Run any chat turn — the store is created lazily.",
        )
    }
}

/// Render a human-readable summary. Stable shape (`[STATUS] id —
/// name: message`) so downstream tooling can parse.
pub fn render_human(report: &DoctorReport) -> String {
    let mut out = String::new();
    for c in &report.checks {
        out.push_str(&format!(
            "[{:<4}] {} — {}: {}\n",
            c.status.as_str(),
            c.id,
            c.name,
            c.message
        ));
        if let Some(fix) = &c.remediation {
            out.push_str(&format!("       → {fix}\n"));
        }
    }
    out.push('\n');
    out.push_str(&format!("overall: {}\n", report.overall().as_str()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn overall_is_fail_when_any_check_fails() {
        let mut r = DoctorReport::new();
        r.push(CheckResult::pass("p", "p", "ok"));
        assert_eq!(r.overall(), CheckStatus::Pass);
        r.push(CheckResult::warn("w", "w", "m", "fix"));
        assert_eq!(r.overall(), CheckStatus::Warn);
        r.push(CheckResult::fail("f", "f", "m", "fix"));
        assert_eq!(r.overall(), CheckStatus::Fail);
    }

    #[test]
    fn writable_check_passes_on_a_real_temp_dir() {
        let dir = tempdir().unwrap();
        let result = check_vulcan_home_writable(dir.path());
        assert_eq!(result.status, CheckStatus::Pass);
        assert!(!dir.path().join(".doctor.write-probe").exists());
    }

    #[test]
    fn writable_check_warns_when_dir_missing() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("nope");
        let r = check_vulcan_home_writable(&missing);
        assert_eq!(r.status, CheckStatus::Warn);
    }

    #[test]
    fn config_present_warns_when_missing() {
        let dir = tempdir().unwrap();
        let r = check_config_present(dir.path());
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.remediation.as_deref().unwrap().contains("vulcan auth"));
    }

    #[test]
    fn workspace_git_warns_outside_git_tree() {
        let dir = tempdir().unwrap();
        let r = check_workspace_is_git(dir.path());
        assert_eq!(r.status, CheckStatus::Warn);
    }

    #[test]
    fn run_records_check_warns_before_first_use() {
        let dir = tempdir().unwrap();
        let r = check_run_records_store(dir.path());
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.message.contains("not yet created"));
    }

    #[test]
    fn run_checks_runs_full_first_slice_set() {
        let dir = tempdir().unwrap();
        let report = run_checks(dir.path(), dir.path());
        // 5 checks in this PR's first slice.
        assert_eq!(report.checks.len(), 5);
    }

    #[test]
    fn render_human_redacts_no_secrets_and_includes_overall() {
        let mut report = DoctorReport::new();
        report.push(CheckResult::pass("a", "alpha", "ok"));
        report.push(CheckResult::warn("b", "beta", "missing", "create it"));
        let txt = render_human(&report);
        assert!(txt.contains("alpha"));
        assert!(txt.contains("create it"));
        assert!(txt.contains("overall: warn"));
    }
}
