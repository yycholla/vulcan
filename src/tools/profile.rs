//! YYC-181: tool capability profiles — named tool sets for agents,
//! lanes, and subagents.
//!
//! A profile is a label + a tool-name allowlist. Applying a profile
//! drops every tool not in the list from a [`ToolRegistry`], so the
//! agent loop, definitions list, and `execute` lookup all see the
//! same restricted surface. The profile name is recorded on the
//! registry so future PRs (run records, doctor output, subagent
//! inheritance) can read it without re-deriving the set.
//!
//! ## Scope of this PR
//!
//! - `ToolProfile` data type + built-in catalog (`readonly`,
//!   `coding`, `reviewer`, `gateway-safe`).
//! - `ToolRegistry::apply_profile` to filter the registry against a
//!   profile and remember the profile name.
//! - `BUILTIN_PROFILES` lookup for the name → profile resolution
//!   future PRs (CLI flag, config) will use.
//!
//! ## Deliberately deferred
//!
//! - User-defined profiles in config (`[profiles.<name>]`).
//! - CLI `--profile` flag + per-session default in config.
//! - Structured denial path through hooks (today: tool just isn't
//!   present in the registry, so the LLM can't request it).
//! - Subagent profile inheritance/narrowing.
//! - Run-record + doctor visibility.

use std::borrow::Cow;

/// A named subset of tool capabilities. Apply via
/// [`crate::tools::ToolRegistry::apply_profile`].
#[derive(Debug, Clone)]
pub struct ToolProfile {
    pub name: Cow<'static, str>,
    pub description: Cow<'static, str>,
    /// Tool names this profile allows. Tools not in the parent
    /// registry are silently dropped on apply — the registry is
    /// the source of truth for what *can* exist; the profile only
    /// narrows it.
    pub allowed: Vec<Cow<'static, str>>,
}

impl ToolProfile {
    pub fn allows(&self, tool_name: &str) -> bool {
        self.allowed.iter().any(|t| t == tool_name)
    }
}

/// Profile names users can refer to in config and the CLI. Kept as
/// `&'static str` so callers can match on them in stable code.
pub const PROFILE_READONLY: &str = "readonly";
pub const PROFILE_CODING: &str = "coding";
pub const PROFILE_REVIEWER: &str = "reviewer";
pub const PROFILE_GATEWAY_SAFE: &str = "gateway-safe";

/// Read-only inspection set: search/read files, structural code
/// queries, LSP navigation, git status — no mutation, no shell, no
/// child agents.
fn readonly_profile() -> ToolProfile {
    ToolProfile {
        name: PROFILE_READONLY.into(),
        description: "Read-only inspection: file reads, code structure queries, LSP navigation, \
            and git status. No writes, no shell, no subagents."
            .into(),
        allowed: [
            "read_file",
            "list_files",
            "search_files",
            "code_outline",
            "code_extract",
            "code_query",
            "find_symbol",
            "goto_definition",
            "find_references",
            "hover",
            "type_definition",
            "implementation",
            "workspace_symbol",
            "call_hierarchy",
            "diagnostics",
            "code_action",
            "git_status",
            "git_diff",
            "git_log",
            "git_branch",
            "web_search",
            "web_fetch",
        ]
        .into_iter()
        .map(Cow::from)
        .collect(),
    }
}

/// Full coding profile: everything the day-to-day agent needs to
/// edit files, run cargo, drive bounded shell commands, and commit.
fn coding_profile() -> ToolProfile {
    let mut tools = readonly_profile().allowed;
    tools.extend(
        [
            "write_file",
            "edit_file",
            "replace_function_body",
            "rename_symbol",
            "cargo_check",
            "index_code_graph",
            "git_commit",
            "git_push",
            "bash",
            "spawn_subagent",
            "ask_user",
        ]
        .into_iter()
        .map(Cow::from),
    );
    ToolProfile {
        name: PROFILE_CODING.into(),
        description: "Day-to-day coding: read/write files, structural edits, cargo, git, bounded \
            shell, subagents."
            .into(),
        allowed: tools,
    }
}

/// Reviewer profile: read-only + diff inspection + non-mutating
/// checks (cargo). Safe for plan/patch review passes.
fn reviewer_profile() -> ToolProfile {
    let mut tools = readonly_profile().allowed;
    tools.extend(["cargo_check", "ask_user"].into_iter().map(Cow::from));
    ToolProfile {
        name: PROFILE_REVIEWER.into(),
        description: "Review mode: read code + diffs and run non-mutating checks like \
            `cargo_check`. No writes."
            .into(),
        allowed: tools,
    }
}

/// Gateway lane safe-set: minimal tools an inbound platform agent
/// needs without granting workspace mutation. Reads + searches but
/// no shell, no edits, no commits.
fn gateway_safe_profile() -> ToolProfile {
    ToolProfile {
        name: PROFILE_GATEWAY_SAFE.into(),
        description: "Gateway lanes: file reads, structural code queries, search, web. No \
            writes, no shell, no git mutation."
            .into(),
        allowed: [
            "read_file",
            "list_files",
            "search_files",
            "code_outline",
            "code_extract",
            "code_query",
            "find_symbol",
            "goto_definition",
            "find_references",
            "hover",
            "diagnostics",
            "git_status",
            "git_diff",
            "git_log",
            "web_search",
            "web_fetch",
        ]
        .into_iter()
        .map(Cow::from)
        .collect(),
    }
}

/// All built-in profiles in catalog order. Used by future CLI/config
/// resolution and by tests pinning the catalog shape.
pub fn builtin_profiles() -> Vec<ToolProfile> {
    vec![
        readonly_profile(),
        coding_profile(),
        reviewer_profile(),
        gateway_safe_profile(),
    ]
}

/// Resolve a profile name (e.g. from CLI / config) to a built-in
/// profile. Returns `None` for unknown names — the caller decides
/// whether to fall back to the unrestricted registry or surface an
/// error.
pub fn builtin_profile(name: &str) -> Option<ToolProfile> {
    match name {
        PROFILE_READONLY => Some(readonly_profile()),
        PROFILE_CODING => Some(coding_profile()),
        PROFILE_REVIEWER => Some(reviewer_profile()),
        PROFILE_GATEWAY_SAFE => Some(gateway_safe_profile()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_excludes_mutating_tools() {
        let p = readonly_profile();
        for forbidden in [
            "write_file",
            "edit_file",
            "bash",
            "git_commit",
            "git_push",
            "spawn_subagent",
            "rename_symbol",
            "replace_function_body",
        ] {
            assert!(
                !p.allows(forbidden),
                "readonly should not allow {forbidden:?}"
            );
        }
        // ...and includes the navigators.
        for must in ["read_file", "goto_definition", "git_diff"] {
            assert!(p.allows(must), "readonly should allow {must:?}");
        }
    }

    #[test]
    fn coding_is_strict_superset_of_readonly() {
        let r = readonly_profile();
        let c = coding_profile();
        for tool in &r.allowed {
            assert!(
                c.allows(tool),
                "coding profile missing readonly tool: {tool}"
            );
        }
        // Plus the mutating set.
        for must in [
            "write_file",
            "edit_file",
            "cargo_check",
            "bash",
            "git_commit",
        ] {
            assert!(c.allows(must), "coding should allow {must:?}");
        }
    }

    #[test]
    fn reviewer_allows_cargo_check_but_not_writes() {
        let p = reviewer_profile();
        assert!(p.allows("cargo_check"));
        assert!(p.allows("git_diff"));
        assert!(!p.allows("write_file"));
        assert!(!p.allows("bash"));
        assert!(!p.allows("git_commit"));
    }

    #[test]
    fn gateway_safe_allows_no_workspace_mutation() {
        let p = gateway_safe_profile();
        for forbidden in [
            "write_file",
            "edit_file",
            "bash",
            "git_commit",
            "git_push",
            "spawn_subagent",
            "cargo_check",
        ] {
            assert!(
                !p.allows(forbidden),
                "gateway-safe should not allow {forbidden:?}"
            );
        }
        assert!(p.allows("read_file"));
        assert!(p.allows("git_status"));
    }

    #[test]
    fn builtin_profile_lookup_returns_known_names() {
        assert!(builtin_profile(PROFILE_READONLY).is_some());
        assert!(builtin_profile(PROFILE_CODING).is_some());
        assert!(builtin_profile(PROFILE_REVIEWER).is_some());
        assert!(builtin_profile(PROFILE_GATEWAY_SAFE).is_some());
        assert!(builtin_profile("nonsense").is_none());
    }

    #[test]
    fn builtin_profiles_returns_full_catalog_with_unique_names() {
        let names: Vec<String> = builtin_profiles()
            .into_iter()
            .map(|p| p.name.to_string())
            .collect();
        assert_eq!(names.len(), 4);
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 4, "profile names must be unique: {names:?}");
    }
}
