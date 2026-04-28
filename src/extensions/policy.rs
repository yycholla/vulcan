//! YYC-236 (YYC-169 PR-1): extension permission types + local
//! policy engine.
//!
//! Pure decision logic — no hook wiring or audit emission.
//! `ExtensionPolicyEngine::decide` is deterministic and testable
//! without any I/O. Subsequent PRs (YYC-169 PR-2/PR-3) wire the
//! engine into tool dispatch and add an audit stream.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// What an extension might want to do at runtime. Manifest must
/// declare each permission it intends to use; the engine refuses
/// any request the manifest didn't pre-declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionPermission {
    FilesystemRead,
    FilesystemWrite,
    Network,
    Shell,
    ProcessSpawn,
    SecretAccess,
    McpLaunch,
    PersistentState,
}

impl ExtensionPermission {
    pub fn as_str(self) -> &'static str {
        match self {
            ExtensionPermission::FilesystemRead => "filesystem_read",
            ExtensionPermission::FilesystemWrite => "filesystem_write",
            ExtensionPermission::Network => "network",
            ExtensionPermission::Shell => "shell",
            ExtensionPermission::ProcessSpawn => "process_spawn",
            ExtensionPermission::SecretAccess => "secret_access",
            ExtensionPermission::McpLaunch => "mcp_launch",
            ExtensionPermission::PersistentState => "persistent_state",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "filesystem_read" => Some(ExtensionPermission::FilesystemRead),
            "filesystem_write" => Some(ExtensionPermission::FilesystemWrite),
            "network" => Some(ExtensionPermission::Network),
            "shell" => Some(ExtensionPermission::Shell),
            "process_spawn" => Some(ExtensionPermission::ProcessSpawn),
            "secret_access" => Some(ExtensionPermission::SecretAccess),
            "mcp_launch" => Some(ExtensionPermission::McpLaunch),
            "persistent_state" => Some(ExtensionPermission::PersistentState),
            _ => None,
        }
    }

    /// True for permissions the default-deny baseline escalates
    /// to `RequireApproval` rather than allowing outright. Picked
    /// to mirror the existing SafetyHook posture: shell, network,
    /// process spawn, MCP launch, and secret access never run
    /// silently on first request.
    fn is_sensitive(self) -> bool {
        matches!(
            self,
            ExtensionPermission::Shell
                | ExtensionPermission::Network
                | ExtensionPermission::ProcessSpawn
                | ExtensionPermission::SecretAccess
                | ExtensionPermission::McpLaunch
        )
    }
}

/// Outcome of a policy check. The engine returns one of these for
/// every requested permission. `RequireApproval` carries a short
/// reason so the future approval UI can render a clear prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PolicyDecision {
    Allow,
    Deny { reason: String },
    RequireApproval { reason: String },
    AllowWithRedaction { reason: String },
    AllowWithQuota { reason: String, limit: u32 },
}

impl PolicyDecision {
    pub fn is_allow(&self) -> bool {
        matches!(
            self,
            PolicyDecision::Allow
                | PolicyDecision::AllowWithRedaction { .. }
                | PolicyDecision::AllowWithQuota { .. }
        )
    }
}

/// Per-extension override that flips a permission's default
/// decision. `Some(decision)` wins over the baseline; `None`
/// (or absent entry) falls through to the default.
#[derive(Debug, Clone, Default)]
pub struct ExtensionPolicyOverride {
    pub per_permission: BTreeMap<ExtensionPermission, PolicyDecision>,
}

#[derive(Debug, Default)]
pub struct ExtensionPolicyEngine {
    overrides: BTreeMap<String, ExtensionPolicyOverride>,
}

impl ExtensionPolicyEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_override(
        &mut self,
        extension_id: impl Into<String>,
        permission: ExtensionPermission,
        decision: PolicyDecision,
    ) {
        let id = extension_id.into();
        let entry = self.overrides.entry(id).or_default();
        entry.per_permission.insert(permission, decision);
    }

    /// Decide whether `extension_id` may exercise `requested`.
    /// `declared` is the set of permissions the manifest
    /// pre-declared. Anything not in `declared` is denied
    /// outright, regardless of overrides.
    pub fn decide(
        &self,
        extension_id: &str,
        declared: &BTreeSet<ExtensionPermission>,
        requested: ExtensionPermission,
    ) -> PolicyDecision {
        if !declared.contains(&requested) {
            return PolicyDecision::Deny {
                reason: format!(
                    "extension `{extension_id}` did not declare permission `{}` in its manifest",
                    requested.as_str()
                ),
            };
        }
        if let Some(per_ext) = self.overrides.get(extension_id) {
            if let Some(decision) = per_ext.per_permission.get(&requested) {
                return decision.clone();
            }
        }
        if requested.is_sensitive() {
            PolicyDecision::RequireApproval {
                reason: format!(
                    "permission `{}` is sensitive; user must approve before use",
                    requested.as_str()
                ),
            }
        } else {
            PolicyDecision::Allow
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(perms: &[ExtensionPermission]) -> BTreeSet<ExtensionPermission> {
        perms.iter().copied().collect()
    }

    #[test]
    fn undeclared_permission_is_denied_even_with_override() {
        let mut engine = ExtensionPolicyEngine::new();
        engine.set_override(
            "lint-helper",
            ExtensionPermission::Shell,
            PolicyDecision::Allow,
        );
        let decision = engine.decide(
            "lint-helper",
            &declared(&[ExtensionPermission::FilesystemRead]),
            ExtensionPermission::Shell,
        );
        assert!(matches!(decision, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn declared_non_sensitive_default_allows() {
        let engine = ExtensionPolicyEngine::new();
        let decision = engine.decide(
            "lint-helper",
            &declared(&[ExtensionPermission::FilesystemRead]),
            ExtensionPermission::FilesystemRead,
        );
        assert_eq!(decision, PolicyDecision::Allow);
    }

    #[test]
    fn declared_sensitive_default_requires_approval() {
        let engine = ExtensionPolicyEngine::new();
        let decision = engine.decide(
            "release-helper",
            &declared(&[ExtensionPermission::Shell, ExtensionPermission::Network]),
            ExtensionPermission::Shell,
        );
        assert!(matches!(decision, PolicyDecision::RequireApproval { .. }));
    }

    #[test]
    fn override_can_promote_sensitive_to_allow() {
        let mut engine = ExtensionPolicyEngine::new();
        engine.set_override(
            "trusted-helper",
            ExtensionPermission::Shell,
            PolicyDecision::Allow,
        );
        let decision = engine.decide(
            "trusted-helper",
            &declared(&[ExtensionPermission::Shell]),
            ExtensionPermission::Shell,
        );
        assert_eq!(decision, PolicyDecision::Allow);
    }

    #[test]
    fn override_can_demote_non_sensitive_to_deny() {
        let mut engine = ExtensionPolicyEngine::new();
        engine.set_override(
            "noisy-extension",
            ExtensionPermission::FilesystemRead,
            PolicyDecision::Deny {
                reason: "operator banned read for this extension".into(),
            },
        );
        let decision = engine.decide(
            "noisy-extension",
            &declared(&[ExtensionPermission::FilesystemRead]),
            ExtensionPermission::FilesystemRead,
        );
        assert!(matches!(decision, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn quota_decision_round_trips_through_serde_json() {
        let d = PolicyDecision::AllowWithQuota {
            reason: "shared budget".into(),
            limit: 50,
        };
        let json = serde_json::to_string(&d).unwrap();
        let back: PolicyDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn permission_string_round_trip_covers_every_variant() {
        for p in [
            ExtensionPermission::FilesystemRead,
            ExtensionPermission::FilesystemWrite,
            ExtensionPermission::Network,
            ExtensionPermission::Shell,
            ExtensionPermission::ProcessSpawn,
            ExtensionPermission::SecretAccess,
            ExtensionPermission::McpLaunch,
            ExtensionPermission::PersistentState,
        ] {
            let s = p.as_str();
            assert_eq!(ExtensionPermission::parse(s), Some(p));
        }
        assert!(ExtensionPermission::parse("nope").is_none());
    }

    #[test]
    fn is_allow_classifies_correctly() {
        assert!(PolicyDecision::Allow.is_allow());
        assert!(
            PolicyDecision::AllowWithQuota {
                reason: "x".into(),
                limit: 1
            }
            .is_allow()
        );
        assert!(
            !PolicyDecision::Deny {
                reason: "no".into()
            }
            .is_allow()
        );
        assert!(
            !PolicyDecision::RequireApproval {
                reason: "ask".into()
            }
            .is_allow()
        );
    }
}
