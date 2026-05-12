//! YYC-185: policy simulation — explain which tools would be
//! allowed, denied, or approval-gated for a given workspace +
//! capability profile combination, without executing anything.
//!
//! Reuses the same resolution code the agent runs at session
//! start (workspace trust → capability profile → tool registry
//! filter) so the simulation matches enforcement to the byte.

use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;

use crate::config::{ApprovalConfig, ApprovalMode, Config};
use crate::tools::{ToolContext, ToolProfile, ToolRegistry};
use crate::trust::TrustProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecision {
    Allowed,
    ApprovalRequired,
    Denied,
}

impl PolicyDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            PolicyDecision::Allowed => "allowed",
            PolicyDecision::ApprovalRequired => "approval_required",
            PolicyDecision::Denied => "denied",
        }
    }
}

/// One tool's resolved policy entry. `source` carries the human
/// explanation of why the decision landed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PolicyEntry {
    pub tool: String,
    pub decision: PolicyDecision,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PolicyWarning {
    pub category: String,
    pub message: String,
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HookSummary {
    pub id: String,
    pub event: String,
    pub enabled: bool,
    pub policy: String,
    pub match_tool: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustOverride {
    pub level: crate::trust::TrustLevel,
    pub capability_profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PolicyDecisionChange {
    pub tool: String,
    pub before: PolicyDecision,
    pub after: PolicyDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PolicyDryRun {
    pub baseline: PolicySimulation,
    pub proposed: PolicySimulation,
    pub tool_changes: Vec<PolicyDecisionChange>,
    pub trust_changed: bool,
    pub hooks_changed: bool,
}

/// Result of a single `simulate` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PolicySimulation {
    pub workspace: String,
    pub trust: TrustProfile,
    pub effective_profile: Option<String>,
    pub effective_profile_source: String,
    pub entries: Vec<PolicyEntry>,
    pub warnings: Vec<PolicyWarning>,
    pub hooks: Vec<HookSummary>,
}

impl PolicySimulation {
    pub fn allowed(&self) -> Vec<&PolicyEntry> {
        self.entries
            .iter()
            .filter(|e| e.decision == PolicyDecision::Allowed)
            .collect()
    }

    pub fn approval_required(&self) -> Vec<&PolicyEntry> {
        self.entries
            .iter()
            .filter(|e| e.decision == PolicyDecision::ApprovalRequired)
            .collect()
    }

    pub fn denied(&self) -> Vec<&PolicyEntry> {
        self.entries
            .iter()
            .filter(|e| e.decision == PolicyDecision::Denied)
            .collect()
    }
}

/// Simulate which tools would be exposed if the agent started in
/// `workspace_root` with optional `profile_override`. The
/// universe of tools is supplied as `available_tools` so callers
/// can plug in either the live registry's tool names or a fixed
/// catalog for tests.
pub fn simulate(
    config: &Config,
    workspace_root: &Path,
    profile_override: Option<&str>,
    available_tools: &[String],
) -> PolicySimulation {
    simulate_with_trust(
        config,
        workspace_root,
        profile_override,
        None,
        available_tools,
    )
}

pub fn simulate_dry_run(
    config: &Config,
    workspace_root: &Path,
    proposed_profile: Option<&str>,
    proposed_trust: Option<TrustOverride>,
    available_tools: &[String],
) -> PolicyDryRun {
    let baseline = simulate(config, workspace_root, None, available_tools);
    let proposed = simulate_with_trust(
        config,
        workspace_root,
        proposed_profile,
        proposed_trust,
        available_tools,
    );
    let tool_changes = diff_tool_decisions(&baseline, &proposed);
    let trust_changed = baseline.trust != proposed.trust;
    let hooks_changed = baseline.hooks != proposed.hooks;
    PolicyDryRun {
        baseline,
        proposed,
        tool_changes,
        trust_changed,
        hooks_changed,
    }
}

fn simulate_with_trust(
    config: &Config,
    workspace_root: &Path,
    profile_override: Option<&str>,
    trust_override: Option<TrustOverride>,
    available_tools: &[String],
) -> PolicySimulation {
    let (trust, trust_default_can_apply) = match trust_override {
        Some(override_) => {
            let mut profile = TrustProfile::for_level_with_reason(
                override_.level,
                "proposed dry-run trust override (not persisted)",
            );
            if let Some(capability_profile) = override_.capability_profile {
                profile.capability_profile = capability_profile;
            }
            (profile, true)
        }
        None => {
            let profile = config.workspace_trust.resolve_for(workspace_root);
            let matched = profile.reason.contains("matched");
            (profile, matched)
        }
    };
    let tool_context = ToolContext::probe(workspace_root.to_path_buf());
    let relevant_tools = relevant_tool_names(workspace_root, &tool_context);

    // Mirror Agent::build_from_parts precedence:
    //   CLI override > tools.profile in config > trust default.
    let (effective_profile_name, source) = if let Some(name) = profile_override {
        (Some(name.to_string()), format!("CLI flag --profile {name}"))
    } else if let Some(name) = config.tools.profile.clone() {
        (Some(name.clone()), format!("[tools] profile = {name}"))
    } else if trust_default_can_apply {
        (
            Some(trust.capability_profile.clone()),
            format!(
                "trust profile (level={}) default capability",
                trust.level.as_str()
            ),
        )
    } else {
        (
            None,
            "no profile override; agent runs with the unrestricted registry".to_string(),
        )
    };

    let resolved_profile: Option<ToolProfile> = effective_profile_name
        .as_deref()
        .and_then(|name| config.tools.resolve_profile(name));
    let approval_cfg = effective_approval_config(config);

    let mut entries = Vec::with_capacity(available_tools.len());
    for tool in available_tools {
        let entry = if !relevant_tools.contains(tool) {
            PolicyEntry {
                tool: tool.clone(),
                decision: PolicyDecision::Denied,
                source: "dropped by workspace context (tool is not relevant here)".to_string(),
            }
        } else {
            match &resolved_profile {
                Some(profile) if !profile.allows(tool) => PolicyEntry {
                    tool: tool.clone(),
                    decision: PolicyDecision::Denied,
                    source: format!("dropped by profile `{}`", profile.name),
                },
                Some(profile) => decision_after_approval(
                    tool,
                    &approval_cfg,
                    &format!("allowed by profile `{}`", profile.name),
                ),
                None => decision_after_approval(tool, &approval_cfg, "no profile applied"),
            }
        };
        entries.push(entry);
    }

    let warnings = collect_warnings(&entries);
    let hooks = collect_hook_summaries(config);

    PolicySimulation {
        workspace: workspace_root.display().to_string(),
        trust,
        effective_profile: effective_profile_name,
        effective_profile_source: source,
        entries,
        warnings,
        hooks,
    }
}

fn collect_hook_summaries(config: &Config) -> Vec<HookSummary> {
    config
        .hooks
        .iter()
        .map(|hook| HookSummary {
            id: hook.id.clone(),
            event: hook.event.as_str().to_string(),
            enabled: hook.enabled,
            policy: match hook.policy {
                crate::hooks::external::ExternalHookPolicy::Allow => "allow",
                crate::hooks::external::ExternalHookPolicy::Deny => "deny",
                crate::hooks::external::ExternalHookPolicy::RequireApproval => "require_approval",
            }
            .to_string(),
            match_tool: hook.match_rule.tool.clone(),
        })
        .collect()
}

fn diff_tool_decisions(
    baseline: &PolicySimulation,
    proposed: &PolicySimulation,
) -> Vec<PolicyDecisionChange> {
    let before: std::collections::BTreeMap<&str, PolicyDecision> = baseline
        .entries
        .iter()
        .map(|entry| (entry.tool.as_str(), entry.decision))
        .collect();
    proposed
        .entries
        .iter()
        .filter_map(|entry| {
            let prior = before.get(entry.tool.as_str())?;
            (*prior != entry.decision).then(|| PolicyDecisionChange {
                tool: entry.tool.clone(),
                before: *prior,
                after: entry.decision,
            })
        })
        .collect()
}

fn effective_approval_config(config: &Config) -> ApprovalConfig {
    let mut approval_cfg = config.tools.approval.clone();
    if config.tools.yolo_mode {
        approval_cfg.default = ApprovalMode::Always;
    }
    approval_cfg
}

fn decision_after_approval(tool: &str, approval_cfg: &ApprovalConfig, base: &str) -> PolicyEntry {
    let (decision, suffix) = match approval_cfg.mode_for(tool) {
        ApprovalMode::Always => (
            PolicyDecision::Allowed,
            "approval mode = always".to_string(),
        ),
        ApprovalMode::Ask => (
            PolicyDecision::ApprovalRequired,
            "approval required by tools.approval (mode = ask)".to_string(),
        ),
        ApprovalMode::Session => (
            PolicyDecision::ApprovalRequired,
            "approval required by tools.approval (mode = session)".to_string(),
        ),
    };

    PolicyEntry {
        tool: tool.to_string(),
        decision,
        source: format!("{base}; {suffix}"),
    }
}

fn relevant_tool_names(workspace_root: &Path, tool_context: &ToolContext) -> BTreeSet<String> {
    let mut registry =
        ToolRegistry::new_with_diff_and_lsp(None, None, workspace_root.to_path_buf());
    registry.filter_for_context(tool_context);
    registry
        .definitions_with_context(Some(tool_context))
        .into_iter()
        .map(|def| def.function.name)
        .collect()
}

/// Default tool universe — the names the live registry registers
/// at startup. Pulled from `ToolRegistry::new` so the simulator
/// stays in lockstep without hand-maintained lists.
pub fn default_tool_universe() -> Vec<String> {
    let registry = ToolRegistry::new();
    registry
        .definitions()
        .into_iter()
        .map(|d| d.function.name)
        .collect()
}

fn collect_warnings(entries: &[PolicyEntry]) -> Vec<PolicyWarning> {
    let active_tools: BTreeSet<&str> = entries
        .iter()
        .filter(|entry| entry.decision != PolicyDecision::Denied)
        .map(|entry| entry.tool.as_str())
        .collect();
    let mut warnings = Vec::new();

    push_warning(
        &mut warnings,
        &active_tools,
        "filesystem",
        "Broad filesystem read access is available.",
        &[
            "read_file",
            "list_files",
            "search_files",
            "code_outline",
            "code_extract",
            "code_query",
        ],
    );
    push_warning(
        &mut warnings,
        &active_tools,
        "persistent_state",
        "Persistent workspace mutation is available.",
        &[
            "write_file",
            "edit_file",
            "replace_function_body",
            "add_method",
            "add_import",
            "rename_symbol",
            "git_commit",
            "git_push",
            "index_code_graph",
        ],
    );
    push_warning(
        &mut warnings,
        &active_tools,
        "network",
        "Network or remote-system access is available.",
        &["web_search", "web_fetch", "git_push"],
    );
    push_warning(
        &mut warnings,
        &active_tools,
        "shell",
        "Shell command execution is available.",
        &["bash"],
    );
    push_warning(
        &mut warnings,
        &active_tools,
        "secrets",
        "Some allowed capabilities can expose environment variables or secrets.",
        &["bash"],
    );

    warnings
}

fn push_warning(
    warnings: &mut Vec<PolicyWarning>,
    active_tools: &BTreeSet<&str>,
    category: &str,
    message: &str,
    candidates: &[&str],
) {
    let tools: Vec<String> = candidates
        .iter()
        .filter(|tool| active_tools.contains(**tool))
        .map(|tool| (*tool).to_string())
        .collect();
    if !tools.is_empty() {
        warnings.push(PolicyWarning {
            category: category.to_string(),
            message: message.to_string(),
            tools,
        });
    }
}

pub fn render_dry_run_markdown(dry_run: &PolicyDryRun) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Policy dry-run: {}\n\n",
        dry_run.proposed.workspace
    ));
    out.push_str(
        "This is a redacted simulation. Proposed profile/trust values are not written to config or run records by this command.\n\n",
    );
    out.push_str("## Proposed changes\n\n");
    if dry_run.tool_changes.is_empty() && !dry_run.trust_changed && !dry_run.hooks_changed {
        out.push_str("- No effective policy changes detected.\n\n");
    } else {
        if dry_run.trust_changed {
            out.push_str(&format!(
                "- trust: {} -> {}\n",
                dry_run.baseline.trust.level.as_str(),
                dry_run.proposed.trust.level.as_str()
            ));
            out.push_str(&format!(
                "- trust capability profile: {} -> {}\n",
                dry_run.baseline.trust.capability_profile,
                dry_run.proposed.trust.capability_profile
            ));
        }
        for change in &dry_run.tool_changes {
            out.push_str(&format!(
                "- tool `{}`: {} -> {}\n",
                change.tool,
                change.before.as_str(),
                change.after.as_str()
            ));
        }
        if dry_run.hooks_changed {
            out.push_str("- hooks: proposed hook set differs from baseline\n");
        }
        out.push('\n');
    }
    out.push_str("## Proposed effective policy\n\n");
    out.push_str(&render_markdown(&dry_run.proposed));
    out
}

pub fn render_markdown(sim: &PolicySimulation) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Policy simulation: {}\n\n", sim.workspace));
    out.push_str(&format!(
        "- trust level: **{}**\n",
        sim.trust.level.as_str()
    ));
    out.push_str(&format!("  - reason: {}\n", sim.trust.reason));
    out.push_str(&format!(
        "  - allow_indexing: {}, allow_persistence: {}\n",
        sim.trust.allow_indexing, sim.trust.allow_persistence
    ));
    out.push_str(&format!(
        "- effective profile: {}\n",
        sim.effective_profile.as_deref().unwrap_or("(none)")
    ));
    out.push_str(&format!("  - source: {}\n\n", sim.effective_profile_source));

    if !sim.warnings.is_empty() {
        out.push_str(&format!("## Warnings ({})\n\n", sim.warnings.len()));
        for warning in &sim.warnings {
            out.push_str(&format!(
                "- [{}] {} Tools: {}\n",
                warning.category,
                warning.message,
                warning.tools.join(", ")
            ));
        }
        out.push('\n');
    }

    if !sim.hooks.is_empty() {
        out.push_str(&format!("## Hooks ({})\n\n", sim.hooks.len()));
        for hook in &sim.hooks {
            let match_tool = hook.match_tool.as_deref().unwrap_or("*");
            let status = if hook.enabled { "enabled" } else { "disabled" };
            out.push_str(&format!(
                "- `{}` — event={}, match.tool={}, status={}, policy={}\n",
                hook.id, hook.event, match_tool, status, hook.policy
            ));
        }
        out.push('\n');
    }

    let allowed = sim.allowed();
    let approval_required = sim.approval_required();
    let denied = sim.denied();

    out.push_str(&format!("## Allowed ({})\n\n", allowed.len()));
    if allowed.is_empty() {
        out.push_str("_None._\n\n");
    } else {
        for entry in allowed {
            out.push_str(&format!("- `{}` — {}\n", entry.tool, entry.source));
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "## Approval required ({})\n\n",
        approval_required.len()
    ));
    if approval_required.is_empty() {
        out.push_str("_None._\n\n");
    } else {
        for entry in approval_required {
            out.push_str(&format!("- `{}` — {}\n", entry.tool, entry.source));
        }
        out.push('\n');
    }

    out.push_str(&format!("## Denied ({})\n\n", denied.len()));
    if denied.is_empty() {
        out.push_str("_None._\n\n");
    } else {
        for entry in denied {
            out.push_str(&format!("- `{}` — {}\n", entry.tool, entry.source));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::{TrustLevel, WorkspaceTrustConfig, WorkspaceTrustRule};
    use tempfile::tempdir;

    fn cfg_with_trust(rules: Vec<WorkspaceTrustRule>) -> Config {
        let mut cfg = Config::default();
        cfg.workspace_trust = WorkspaceTrustConfig { rules };
        cfg
    }

    fn fixture_universe() -> Vec<String> {
        vec![
            "read_file".into(),
            "list_files".into(),
            "search_files".into(),
            "write_file".into(),
            "edit_file".into(),
            "bash".into(),
            "git_status".into(),
            "git_commit".into(),
        ]
    }

    #[test]
    fn unknown_workspace_falls_back_to_unrestricted_registry() {
        let cfg = cfg_with_trust(Vec::new());
        let dir = tempdir().unwrap();
        let sim = simulate(&cfg, dir.path(), None, &fixture_universe());
        assert!(sim.effective_profile.is_none());
        assert!(sim.allowed().iter().any(|entry| entry.tool == "read_file"));
    }

    #[test]
    fn cli_profile_override_drops_mutating_tools() {
        let cfg = cfg_with_trust(Vec::new());
        let dir = tempdir().unwrap();
        let sim = simulate(&cfg, dir.path(), Some("readonly"), &fixture_universe());
        assert_eq!(sim.effective_profile.as_deref(), Some("readonly"));
        let denied: Vec<&str> = sim.denied().iter().map(|e| e.tool.as_str()).collect();
        for forbidden in ["write_file", "edit_file", "bash", "git_commit"] {
            assert!(
                denied.contains(&forbidden),
                "readonly should deny {forbidden:?}; denied={denied:?}"
            );
        }
        let visible: Vec<&str> = sim
            .entries
            .iter()
            .filter(|e| e.decision != PolicyDecision::Denied)
            .map(|e| e.tool.as_str())
            .collect();
        assert!(visible.contains(&"read_file"));
    }

    #[test]
    fn trust_profile_default_applies_when_no_override() {
        let dir = tempdir().unwrap();
        let cfg = cfg_with_trust(vec![WorkspaceTrustRule {
            path: dir.path().to_path_buf(),
            level: TrustLevel::Sensitive,
            capability_profile: None,
        }]);
        let sim = simulate(&cfg, dir.path(), None, &fixture_universe());
        assert_eq!(sim.effective_profile.as_deref(), Some("readonly"));
        assert!(sim.effective_profile_source.contains("trust profile"));
        let denied: Vec<&str> = sim.denied().iter().map(|e| e.tool.as_str()).collect();
        assert!(denied.contains(&"write_file"));
        assert!(denied.contains(&"bash"));
    }

    #[test]
    fn config_tools_profile_beats_trust_default() {
        let dir = tempdir().unwrap();
        let mut cfg = cfg_with_trust(vec![WorkspaceTrustRule {
            path: dir.path().to_path_buf(),
            level: TrustLevel::Sensitive,
            capability_profile: None,
        }]);
        cfg.tools.profile = Some("coding".into());
        let sim = simulate(&cfg, dir.path(), None, &fixture_universe());
        assert_eq!(sim.effective_profile.as_deref(), Some("coding"));
        let allowed: Vec<&str> = sim.allowed().iter().map(|e| e.tool.as_str()).collect();
        assert!(allowed.contains(&"write_file"));
        assert!(sim.effective_profile_source.contains("[tools] profile"));
    }

    #[test]
    fn render_markdown_includes_section_counts() {
        let cfg = cfg_with_trust(Vec::new());
        let dir = tempdir().unwrap();
        let sim = simulate(&cfg, dir.path(), Some("readonly"), &fixture_universe());
        let md = render_markdown(&sim);
        assert!(md.contains("# Policy simulation:"));
        assert!(md.contains("trust level: **untrusted**"));
        assert!(md.contains("## Allowed"));
        assert!(md.contains("## Approval required"));
        assert!(md.contains("## Denied"));
    }

    #[test]
    fn approval_modes_surface_as_approval_required() {
        let mut cfg = cfg_with_trust(Vec::new());
        cfg.tools.approval.default = crate::config::ApprovalMode::Ask;
        let dir = tempdir().unwrap();
        let sim = simulate(&cfg, dir.path(), Some("readonly"), &fixture_universe());
        let read_file = sim
            .entries
            .iter()
            .find(|entry| entry.tool == "read_file")
            .expect("read_file entry");
        assert_eq!(read_file.decision.as_str(), "approval_required");
        assert!(read_file.source.contains("approval"));
    }

    #[test]
    fn yolo_mode_downgrades_approval_required_to_allowed() {
        let mut cfg = cfg_with_trust(Vec::new());
        cfg.tools.approval.default = crate::config::ApprovalMode::Ask;
        cfg.tools.yolo_mode = true;
        let dir = tempdir().unwrap();
        let sim = simulate(&cfg, dir.path(), Some("readonly"), &fixture_universe());
        let read_file = sim
            .entries
            .iter()
            .find(|entry| entry.tool == "read_file")
            .expect("read_file entry");
        assert_eq!(read_file.decision, PolicyDecision::Allowed);
    }

    #[test]
    fn irrelevant_tools_are_denied_by_workspace_context() {
        let cfg = cfg_with_trust(Vec::new());
        let dir = tempdir().unwrap();
        let sim = simulate(&cfg, dir.path(), None, &default_tool_universe());
        let cargo_check = sim
            .entries
            .iter()
            .find(|entry| entry.tool == "cargo_check")
            .expect("cargo_check entry");
        assert_eq!(cargo_check.decision, PolicyDecision::Denied);
        assert!(cargo_check.source.contains("workspace context"));
    }

    #[test]
    fn warnings_cover_shell_network_and_persistence() {
        let cfg = cfg_with_trust(Vec::new());
        let dir = tempdir().unwrap();
        let sim = simulate(&cfg, dir.path(), Some("coding"), &fixture_universe());
        let categories: Vec<&str> = sim
            .warnings
            .iter()
            .map(|warning| warning.category.as_str())
            .collect();
        assert!(categories.contains(&"filesystem"));
        assert!(categories.contains(&"persistent_state"));
        assert!(categories.contains(&"shell"));
    }

    #[test]
    fn dry_run_reports_tool_delta_for_proposed_profile_without_persisting() {
        let cfg = cfg_with_trust(Vec::new());
        let dir = tempdir().unwrap();
        let dry_run = simulate_dry_run(
            &cfg,
            dir.path(),
            Some("readonly"),
            None,
            &fixture_universe(),
        );
        let changed: Vec<&str> = dry_run
            .tool_changes
            .iter()
            .map(|change| change.tool.as_str())
            .collect();
        assert!(changed.contains(&"write_file"));
        assert!(changed.contains(&"bash"));
        assert_eq!(dry_run.baseline.effective_profile, None);
        assert_eq!(
            dry_run.proposed.effective_profile.as_deref(),
            Some("readonly")
        );
    }

    #[test]
    fn dry_run_reports_workspace_trust_override_differences() {
        let cfg = cfg_with_trust(Vec::new());
        let dir = tempdir().unwrap();
        let dry_run = simulate_dry_run(
            &cfg,
            dir.path(),
            None,
            Some(TrustOverride {
                level: TrustLevel::Trusted,
                capability_profile: Some("coding".into()),
            }),
            &fixture_universe(),
        );
        assert!(dry_run.trust_changed);
        assert_eq!(dry_run.proposed.trust.level, TrustLevel::Trusted);
        assert_eq!(
            dry_run.proposed.effective_profile.as_deref(),
            Some("coding")
        );
        assert!(
            dry_run
                .proposed
                .effective_profile_source
                .contains("trust profile")
        );
    }

    #[test]
    fn hook_summary_is_redacted_and_rendered() {
        let mut cfg = cfg_with_trust(Vec::new());
        cfg.hooks.push(crate::hooks::external::ExternalHookConfig {
            id: "audit-write".into(),
            event: crate::hooks::external::ExternalHookEvent::BeforeToolCall,
            match_rule: crate::hooks::external::ExternalHookMatch {
                tool: Some("write_file".into()),
            },
            command: "/secret/path/hook.sh".into(),
            args: vec!["--token=shh".into()],
            env: std::collections::HashMap::from([("SECRET".into(), "value".into())]),
            enabled: true,
            policy: crate::hooks::external::ExternalHookPolicy::RequireApproval,
            priority: 10,
            timeout_secs: 5,
        });
        let dir = tempdir().unwrap();
        let sim = simulate(&cfg, dir.path(), Some("coding"), &fixture_universe());
        assert_eq!(sim.hooks.len(), 1);
        assert_eq!(sim.hooks[0].id, "audit-write");
        let rendered = render_markdown(&sim);
        assert!(rendered.contains("## Hooks"));
        assert!(rendered.contains("audit-write"));
        assert!(!rendered.contains("/secret/path"));
        assert!(!rendered.contains("--token"));
        assert!(!rendered.contains("SECRET"));
    }
}
