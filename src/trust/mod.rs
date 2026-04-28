//! YYC-182: per-workspace trust profiles.
//!
//! When Vulcan starts in or is pointed at a workspace, the
//! resolver consults user config for a matching rule and returns
//! a [`TrustProfile`]. The profile determines downstream policy:
//! the default tool capability profile, persistence/indexing
//! posture, and (later) approval gating.
//!
//! ## Scope of this PR
//!
//! - `TrustLevel` enum + `TrustProfile` with built-in defaults.
//! - `WorkspaceTrustConfig` for user-supplied rules under
//!   `[workspace_trust]`.
//! - Path-based resolver (longest-prefix wins). Git remote
//!   matching is a follow-up.
//! - Conservative fallback for unknown workspaces.
//!
//! ## Deliberately deferred
//!
//! - Wiring the resolved profile into Agent / capability profile
//!   selection (separate PR).
//! - TUI status indicator.
//! - Run-record annotation (waits on PR-2).
//! - Git remote fingerprint matching.
//! - `vulcan trust list/show/why` CLI surface.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Coarse trust classification. Higher in the list = stricter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustLevel {
    /// Normal coding capabilities — personal repo the user owns.
    Trusted,
    /// Read-first; writes and shell exec require explicit approval.
    Restricted,
    /// Reduced persistence, stricter redaction, no indexing unless
    /// the user opts in explicitly.
    Sensitive,
    /// Read-only by default; no executable scripts without
    /// approval. Default for unknown / freshly cloned third-party
    /// projects.
    Untrusted,
}

impl TrustLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            TrustLevel::Trusted => "trusted",
            TrustLevel::Restricted => "restricted",
            TrustLevel::Sensitive => "sensitive",
            TrustLevel::Untrusted => "untrusted",
        }
    }

    /// Default capability profile for this trust level. The
    /// agent narrows the active profile against this — never
    /// widens it.
    pub fn default_capability_profile(self) -> &'static str {
        match self {
            TrustLevel::Trusted => "coding",
            TrustLevel::Restricted => "reviewer",
            TrustLevel::Sensitive => "readonly",
            TrustLevel::Untrusted => "readonly",
        }
    }
}

/// Resolved profile applied to a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustProfile {
    pub level: TrustLevel,
    /// Tool capability profile name to apply by default.
    /// Resolved from `level.default_capability_profile()` or
    /// overridden by config.
    pub capability_profile: String,
    /// Why this profile resolved — for `vulcan trust why` /
    /// run-record provenance.
    pub reason: String,
    /// Persistence posture. `Sensitive` flips this off so session
    /// memory + run records redact more aggressively.
    pub allow_persistence: bool,
    /// Whether the embedding/code-graph indexers are allowed to
    /// touch this workspace by default. `Untrusted` and
    /// `Sensitive` say no.
    pub allow_indexing: bool,
}

impl TrustProfile {
    pub fn for_level_with_reason(level: TrustLevel, reason: impl Into<String>) -> Self {
        let allow_persistence = matches!(level, TrustLevel::Trusted | TrustLevel::Restricted);
        let allow_indexing = matches!(level, TrustLevel::Trusted | TrustLevel::Restricted);
        Self {
            level,
            capability_profile: level.default_capability_profile().to_string(),
            reason: reason.into(),
            allow_persistence,
            allow_indexing,
        }
    }

    /// Conservative fallback when the resolver can't match the
    /// workspace anywhere. Surface to the user via `reason`.
    pub fn unknown_default() -> Self {
        Self::for_level_with_reason(
            TrustLevel::Untrusted,
            "no matching workspace_trust rule (default: untrusted)",
        )
    }
}

/// User-declared rule. `path` matches by canonical-path prefix
/// (longest match wins). Future: `git_remote` for remote-based
/// matching.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkspaceTrustRule {
    /// Path prefix to match. Tilde / env expansion is the caller's
    /// responsibility today; the resolver compares canonical
    /// `PathBuf`s as-is.
    pub path: PathBuf,
    pub level: TrustLevel,
    /// Optional override of the level's default capability profile.
    #[serde(default)]
    pub capability_profile: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkspaceTrustConfig {
    #[serde(default)]
    pub rules: Vec<WorkspaceTrustRule>,
}

impl Default for TrustLevel {
    fn default() -> Self {
        TrustLevel::Untrusted
    }
}

impl WorkspaceTrustConfig {
    /// Resolve a trust profile for `workspace_root`. Returns the
    /// rule with the longest matching `path` prefix; falls back to
    /// `TrustProfile::unknown_default()` if nothing matches.
    pub fn resolve_for(&self, workspace_root: &Path) -> TrustProfile {
        let canonical_root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());

        let mut best: Option<(usize, &WorkspaceTrustRule)> = None;
        for rule in &self.rules {
            let canonical_rule = rule
                .path
                .canonicalize()
                .unwrap_or_else(|_| rule.path.clone());
            if canonical_root.starts_with(&canonical_rule) {
                let depth = canonical_rule.components().count();
                if best.map(|(d, _)| depth > d).unwrap_or(true) {
                    best = Some((depth, rule));
                }
            }
        }

        match best {
            Some((_, rule)) => {
                let mut profile = TrustProfile::for_level_with_reason(
                    rule.level,
                    format!(
                        "matched [workspace_trust] rule path={}",
                        rule.path.display()
                    ),
                );
                if let Some(custom) = &rule.capability_profile {
                    profile.capability_profile = custom.clone();
                }
                profile
            }
            None => TrustProfile::unknown_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn unknown_workspace_falls_back_to_untrusted() {
        let cfg = WorkspaceTrustConfig::default();
        let dir = tempdir().unwrap();
        let p = cfg.resolve_for(dir.path());
        assert_eq!(p.level, TrustLevel::Untrusted);
        assert_eq!(p.capability_profile, "readonly");
        assert!(p.reason.contains("no matching"));
        assert!(!p.allow_persistence);
        assert!(!p.allow_indexing);
    }

    #[test]
    fn longest_prefix_wins_when_rules_overlap() {
        let dir = tempdir().unwrap();
        let outer = dir.path().to_path_buf();
        let inner = outer.join("subproj");
        std::fs::create_dir_all(&inner).unwrap();

        let cfg = WorkspaceTrustConfig {
            rules: vec![
                WorkspaceTrustRule {
                    path: outer.clone(),
                    level: TrustLevel::Restricted,
                    capability_profile: None,
                },
                WorkspaceTrustRule {
                    path: inner.clone(),
                    level: TrustLevel::Trusted,
                    capability_profile: None,
                },
            ],
        };

        let p = cfg.resolve_for(&inner);
        assert_eq!(p.level, TrustLevel::Trusted);
        let outer_p = cfg.resolve_for(&outer);
        assert_eq!(outer_p.level, TrustLevel::Restricted);
    }

    #[test]
    fn rule_can_override_capability_profile() {
        let dir = tempdir().unwrap();
        let cfg = WorkspaceTrustConfig {
            rules: vec![WorkspaceTrustRule {
                path: dir.path().to_path_buf(),
                level: TrustLevel::Trusted,
                capability_profile: Some("reviewer".into()),
            }],
        };
        let p = cfg.resolve_for(dir.path());
        assert_eq!(p.level, TrustLevel::Trusted);
        assert_eq!(p.capability_profile, "reviewer");
    }

    #[test]
    fn sensitive_level_blocks_indexing_and_persistence() {
        let p = TrustProfile::for_level_with_reason(TrustLevel::Sensitive, "test");
        assert!(!p.allow_indexing);
        assert!(!p.allow_persistence);
        assert_eq!(p.capability_profile, "readonly");
    }

    #[test]
    fn trusted_level_allows_indexing_and_persistence() {
        let p = TrustProfile::for_level_with_reason(TrustLevel::Trusted, "test");
        assert!(p.allow_indexing);
        assert!(p.allow_persistence);
        assert_eq!(p.capability_profile, "coding");
    }
}
