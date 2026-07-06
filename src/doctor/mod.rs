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

use crate::config::Config;
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

    let config = match load_config_for_doctor(vulcan_home_dir, &mut report) {
        Some(config) => config,
        None => {
            report.push(check_workspace_is_git(cwd));
            report.push(check_run_records_store(vulcan_home_dir));
            return report;
        }
    };

    report.push(check_provider_health(&config));
    report.push(check_tool_profile_health(&config));
    report.push(check_gateway_health(&config));
    for check in check_workspace_trust_health(&config, cwd) {
        report.push(check);
    }
    report.push(check_workspace_is_git(cwd));
    report.push(check_run_records_store(vulcan_home_dir));
    report
}

fn load_config_for_doctor(dir: &Path, report: &mut DoctorReport) -> Option<Config> {
    match Config::load_from_dir(dir) {
        Ok(config) => {
            report.push(CheckResult::pass(
                "config.file.parse",
                "config parse",
                "config fragments parsed successfully",
            ));
            Some(config)
        }
        Err(err) => {
            report.push(CheckResult::fail(
                "config.file.parse",
                "config parse",
                redact_secrets(&format!("{err:#}")),
                "Fix the malformed TOML or move it aside, then rerun `vulcan doctor`.",
            ));
            None
        }
    }
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
    let providers = dir.join("providers.toml");
    let keybinds = dir.join("keybinds.toml");
    if cfg.exists() || providers.exists() || keybinds.exists() {
        let mut found = Vec::new();
        if cfg.exists() {
            found.push("config.toml");
        }
        if providers.exists() {
            found.push("providers.toml");
        }
        if keybinds.exists() {
            found.push("keybinds.toml");
        }
        CheckResult::pass(
            "config.file.present",
            "config fragments",
            format!("found {} in {}", found.join(", "), dir.display()),
        )
    } else {
        CheckResult::warn(
            "config.file.present",
            "config fragments",
            format!(
                "missing config.toml/providers.toml/keybinds.toml under {}",
                dir.display()
            ),
            "Run `vulcan auth` to set up provider config, or create config.toml manually.",
        )
    }
}

fn check_provider_health(config: &Config) -> CheckResult {
    if let Some(name) = config.active_profile.as_deref()
        && !config.providers.contains_key(name)
    {
        return CheckResult::fail(
            "provider.active_profile.resolves",
            "active provider profile",
            format!("active_profile `{name}` is not declared in [providers]"),
            "Add the named profile to providers.toml or clear active_profile.",
        );
    }

    let provider = config.active_provider_config();
    if provider.r#type.trim().is_empty() || provider.r#type != "openai-compat" {
        return CheckResult::fail(
            "provider.type.supported",
            "provider type",
            format!("unsupported provider type `{}`", provider.r#type),
            "Set provider.type = `openai-compat` or update the doctor checker when new provider kinds land.",
        );
    }
    if provider.model.trim().is_empty() {
        return CheckResult::fail(
            "provider.model.present",
            "provider model",
            "model is empty",
            "Set provider.model to a non-empty model id.",
        );
    }
    if url::Url::parse(&provider.base_url).is_err() {
        return CheckResult::fail(
            "provider.base_url.valid",
            "provider base_url",
            "base_url is not a valid URL",
            "Set provider.base_url to an absolute URL such as https://openrouter.ai/api/v1.",
        );
    }
    let has_inline_key = provider
        .api_key
        .as_deref()
        .map(|key| !key.trim().is_empty())
        .unwrap_or(false);
    let has_env_key = std::env::var("VULCAN_API_KEY")
        .ok()
        .map(|key| !key.trim().is_empty())
        .unwrap_or(false);
    let has_auth_source = provider
        .auth_source
        .as_deref()
        .map(|source| !source.trim().is_empty())
        .unwrap_or(false);
    if !(has_inline_key || has_env_key || has_auth_source) {
        return CheckResult::fail(
            "provider.credentials.present",
            "provider credentials",
            "no api_key, VULCAN_API_KEY, or auth_source configured",
            "Run `vulcan auth`, set VULCAN_API_KEY, or configure provider.auth_source.",
        );
    }
    CheckResult::pass(
        "provider.credentials.present",
        "provider credentials",
        format!(
            "{} credential source configured for model {}",
            if has_auth_source {
                "auth_source"
            } else {
                "api key"
            },
            provider.model
        ),
    )
}

fn check_tool_profile_health(config: &Config) -> CheckResult {
    match config.tools.profile.as_deref() {
        Some(name) => match config.tools.resolve_profile(name) {
            Some(profile) => CheckResult::pass(
                "tools.profile.resolves",
                "tool capability profile",
                format!("{name} resolves ({} allowed tools)", profile.allowed.len()),
            ),
            None => CheckResult::fail(
                "tools.profile.resolves",
                "tool capability profile",
                format!("unknown tools.profile `{name}`"),
                "Use a built-in profile (readonly, coding, reviewer, gateway-safe) or define [tools.profiles.<name>].",
            ),
        },
        None => CheckResult::warn(
            "tools.profile.resolves",
            "tool capability profile",
            "no explicit tools.profile configured; runtime will derive from workspace trust or defaults",
            "Set [tools] profile = `coding`/`readonly` or rely on workspace_trust defaults intentionally.",
        ),
    }
}

fn check_gateway_health(config: &Config) -> CheckResult {
    match &config.gateway {
        Some(gateway) => match gateway.validate() {
            Ok(()) => CheckResult::pass(
                "gateway.config.valid",
                "gateway config",
                format!("gateway configured on {}", gateway.bind),
            ),
            Err(err) => CheckResult::fail(
                "gateway.config.valid",
                "gateway config",
                redact_secrets(&format!("{err:#}")),
                "Fix [gateway] config or remove the section if gateway mode is not needed.",
            ),
        },
        None => CheckResult::warn(
            "gateway.config.optional",
            "gateway config",
            "gateway is not configured (optional; CLI/TUI remain usable)",
            "Add [gateway] only if you need daemon/platform delivery mode.",
        ),
    }
}

fn check_workspace_trust_health(config: &Config, cwd: &Path) -> Vec<CheckResult> {
    let profile = config.workspace_trust.resolve_for(cwd);
    let trust_status = if profile.reason.contains("no matching") {
        CheckStatus::Warn
    } else {
        CheckStatus::Pass
    };
    let trust = CheckResult {
        id: "workspace.trust.resolution".into(),
        name: "workspace trust resolution".into(),
        status: trust_status,
        message: format!(
            "{} via {}; persistence={}, indexing={}",
            profile.level.as_str(),
            profile.reason,
            profile.allow_persistence,
            profile.allow_indexing
        ),
        remediation: (trust_status == CheckStatus::Warn)
            .then(|| "Add a [[workspace_trust.rules]] entry for this workspace if the default is too restrictive.".into()),
    };
    let capability = match config.tools.resolve_profile(&profile.capability_profile) {
        Some(resolved) => CheckResult::pass(
            "capability.trust_profile.resolves",
            "trust capability profile",
            format!(
                "workspace trust selects `{}` ({} allowed tools)",
                profile.capability_profile,
                resolved.allowed.len()
            ),
        ),
        None => CheckResult::fail(
            "capability.trust_profile.resolves",
            "trust capability profile",
            format!(
                "workspace trust selects unknown capability profile `{}`",
                profile.capability_profile
            ),
            "Use a built-in profile or define the custom profile under [tools.profiles].",
        ),
    };
    vec![trust, capability]
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
        // Best-effort openability — try to open + immediately close.
        match crate::db::block_on(crate::db::open(&path)) {
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
            redact_secrets(&c.message)
        ));
        if let Some(fix) = &c.remediation {
            out.push_str(&format!("       → {}\n", redact_secrets(fix)));
        }
    }
    out.push('\n');
    out.push_str(&format!("overall: {}\n", report.overall().as_str()));
    out
}

fn redact_secrets(input: &str) -> String {
    let mut out = Vec::new();
    for token in input.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        if lower.starts_with("sk-")
            || lower.contains("api_key")
            || lower.contains("api-token")
            || lower.contains("api_token")
            || lower.contains("bot_token")
            || lower.contains("bearer")
        {
            let suffix = token
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_punctuation() && *c != '-' && *c != '_')
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>();
            out.push(format!("<redacted>{suffix}"));
        } else {
            out.push(token.to_string());
        }
    }
    out.join(" ")
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
        let dir = tempfile::Builder::new()
            .prefix("vulcan-doctor-non-git-")
            .tempdir_in("/var/tmp")
            .unwrap();
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
    fn run_checks_reports_required_operability_categories() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"
[provider]
base_url = "https://example.test/v1"
api_key = "sk-secret-do-not-print"
model = "test-model"

[tools]
profile = "coding"

[[workspace_trust.rules]]
path = "."
level = "trusted"
"#,
        )
        .unwrap();
        let report = run_checks(dir.path(), dir.path());
        for prefix in [
            "config.",
            "provider.",
            "storage.",
            "tools.",
            "gateway.",
            "capability.",
            "workspace.trust.",
        ] {
            assert!(
                report
                    .checks
                    .iter()
                    .any(|check| check.id.starts_with(prefix)),
                "missing category prefix {prefix}: {:?}",
                report.checks
            );
        }
        let rendered = render_human(&report);
        assert!(!rendered.contains("sk-secret-do-not-print"));
    }

    #[test]
    fn bad_config_parse_is_a_distinct_failure() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[provider\napi_key = 'oops'",
        )
        .unwrap();
        let report = run_checks(dir.path(), dir.path());
        let check = report
            .checks
            .iter()
            .find(|check| check.id == "config.file.parse")
            .expect("parse check");
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.message.contains("Failed to parse"));
    }

    #[test]
    fn provider_check_fails_without_credentials() {
        let cfg = crate::config::Config::default();
        let check = check_provider_health(&cfg);
        assert_eq!(check.status, CheckStatus::Fail);
        assert_eq!(check.id, "provider.credentials.present");
    }

    #[test]
    fn tool_profile_check_fails_unknown_profile() {
        let mut cfg = crate::config::Config::default();
        cfg.tools.profile = Some("does-not-exist".into());
        let check = check_tool_profile_health(&cfg);
        assert_eq!(check.status, CheckStatus::Fail);
        assert_eq!(check.id, "tools.profile.resolves");
    }

    #[test]
    fn workspace_trust_reports_unknown_workspace_without_failing() {
        let cfg = crate::config::Config::default();
        let dir = tempdir().unwrap();
        let checks = check_workspace_trust_health(&cfg, dir.path());
        assert!(
            checks
                .iter()
                .any(|check| check.id == "workspace.trust.resolution")
        );
        assert!(
            checks
                .iter()
                .any(|check| check.id == "capability.trust_profile.resolves")
        );
        assert!(checks.iter().all(|check| check.status != CheckStatus::Fail));
    }

    #[test]
    fn gateway_absent_is_optional_warning_not_failure() {
        let cfg = crate::config::Config::default();
        let check = check_gateway_health(&cfg);
        assert_eq!(check.id, "gateway.config.optional");
        assert_eq!(check.status, CheckStatus::Warn);
    }

    #[test]
    fn render_human_redacts_no_secrets_and_includes_overall() {
        let mut report = DoctorReport::new();
        report.push(CheckResult::pass("a", "alpha", "ok sk-secret-do-not-print"));
        report.push(CheckResult::warn(
            "b",
            "beta",
            "missing",
            "create secret sk-secret-do-not-print",
        ));
        let txt = render_human(&report);
        assert!(txt.contains("alpha"));
        assert!(txt.contains("overall: warn"));
        assert!(!txt.contains("sk-secret-do-not-print"));
        assert!(txt.contains("<redacted>"));
    }
}
