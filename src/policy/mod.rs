//! YYC-185: policy simulation — explain which tools would be
//! allowed, denied, or approval-gated for a given workspace +
//! capability profile combination, without executing anything.
//!
//! Reuses the same resolution code the agent runs at session
//! start (workspace trust → capability profile → tool registry
//! filter) so the simulation matches enforcement to the byte.
//!
//! ## Scope of this PR
//!
//! - `PolicyDecision` + `PolicySimulation` shapes.
//! - `simulate` function over `Config + workspace_root +
//!   profile_override`.
//! - Source-attribution strings on each decision.
//! - Markdown render for `vulcan policy simulate`.
//!
//! ## Deliberately deferred
//!
//! - MCP / extension capability simulation (waits on YYC-164).
//! - Live hook configuration cross-check.
//! - Approval-mode introspection (YYC-76 ApprovalConfig surface).

use serde::Serialize;
use std::path::Path;

use crate::config::Config;
use crate::tools::{ToolProfile, ToolRegistry, builtin_profile};
use crate::trust::TrustProfile;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecision {
    Allowed,
    Denied,
}

impl PolicyDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            PolicyDecision::Allowed => "allowed",
            PolicyDecision::Denied => "denied",
        }
    }
}

/// One tool's resolved policy entry. `source` carries the human
/// explanation of *why* the decision landed (e.g. "blocked by
/// readonly profile", "allowed by trust=trusted default").
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PolicyEntry {
    pub tool: String,
    pub decision: PolicyDecision,
    pub source: String,
}

/// Result of a single `simulate` call. `Deserialize` not derived
/// because nested `TrustProfile` is serialize-only — round-trip
/// isn't a use case yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PolicySimulation {
    pub workspace: String,
    pub trust: TrustProfile,
    pub effective_profile: Option<String>,
    pub effective_profile_source: String,
    pub entries: Vec<PolicyEntry>,
}

impl PolicySimulation {
    pub fn allowed(&self) -> Vec<&PolicyEntry> {
        self.entries
            .iter()
            .filter(|e| e.decision == PolicyDecision::Allowed)
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
    let trust = config.workspace_trust.resolve_for(workspace_root);

    // Mirror Agent::build_from_parts precedence:
    //   CLI override > tools.profile in config > trust default.
    let (effective_profile_name, source) = if let Some(name) = profile_override {
        (Some(name.to_string()), format!("CLI flag --profile {name}"))
    } else if let Some(name) = config.tools.profile.clone() {
        (Some(name.clone()), format!("[tools] profile = {name}"))
    } else if trust.reason.contains("matched") {
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

    let resolved_profile: Option<ToolProfile> =
        effective_profile_name.as_deref().and_then(|name| {
            config
                .tools
                .resolve_profile(name)
                .or_else(|| builtin_profile(name))
        });

    let mut entries = Vec::with_capacity(available_tools.len());
    for tool in available_tools {
        let entry = match &resolved_profile {
            Some(p) => {
                if p.allows(tool) {
                    PolicyEntry {
                        tool: tool.clone(),
                        decision: PolicyDecision::Allowed,
                        source: format!("allowed by profile `{}`", p.name),
                    }
                } else {
                    PolicyEntry {
                        tool: tool.clone(),
                        decision: PolicyDecision::Denied,
                        source: format!("dropped by profile `{}`", p.name),
                    }
                }
            }
            None => PolicyEntry {
                tool: tool.clone(),
                decision: PolicyDecision::Allowed,
                source: "no profile applied".to_string(),
            },
        };
        entries.push(entry);
    }

    PolicySimulation {
        workspace: workspace_root.display().to_string(),
        trust,
        effective_profile: effective_profile_name,
        effective_profile_source: source,
        entries,
    }
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

    let allowed = sim.allowed();
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
        // Every tool allowed when no profile applies.
        assert_eq!(sim.allowed().len(), fixture_universe().len());
        assert!(sim.denied().is_empty());
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
        let allowed: Vec<&str> = sim.allowed().iter().map(|e| e.tool.as_str()).collect();
        assert!(allowed.contains(&"read_file"));
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
        // Sensitive level → readonly capability default.
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
        // Coding allows write_file even though trust default would
        // have denied it.
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
        assert!(md.contains("## Denied"));
    }
}
