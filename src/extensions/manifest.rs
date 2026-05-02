//! YYC-229 (YYC-166 PR-1): on-disk manifest for an installed
//! extension.
//!
//! `extension.toml` lives at the root of every installed
//! extension directory. This module owns:
//!
//! - the `ExtensionManifest` shape,
//! - the `EntryKind` enum for what the extension is,
//! - typed validation errors so callers can surface actionable
//!   error messages without grepping prose.
//!
//! No I/O happens here yet — discovery walking, install state,
//! and registry wiring land in follow-up YYC-166 children.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum EntryKind {
    /// Compiled into the vulcan binary. The manifest still lives
    /// on disk so per-id configuration + display metadata survive
    /// reinstall.
    Builtin,
    /// Local script extension — `.vpk` packaging tracked
    /// elsewhere; for now this is a path relative to the
    /// extension directory the runtime would invoke. Not loaded
    /// by this PR.
    LocalScript { path: String },
    /// Stub for native dynamic libraries. Recognised so the
    /// parser doesn't fail on a manifest pointing at one, but
    /// loading is explicitly out of scope for the YYC-166 epic.
    NativeLibrary { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub entry: EntryKind,
    /// Free-form capability tags, mirroring the runtime
    /// `ExtensionCapability` set. The store accepts any string;
    /// the registry validates against the live capability enum
    /// when the extension activates.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Frontend capability tags required before this extension can
    /// activate for a frontend-backed session.
    #[serde(default)]
    pub requires_frontend: Vec<String>,
    /// Human-readable permissions summary surfaced in
    /// `vulcan extension list/show`.
    #[serde(default)]
    pub permissions: Option<String>,
    /// Optional content checksum for the extension payload.
    /// Mirrors the future `.vpk` digest field; today we just
    /// preserve it round-trip without enforcing.
    #[serde(default)]
    pub checksum: Option<String>,
    /// Lowest Vulcan version that supports this manifest.
    #[serde(default)]
    pub min_vulcan_version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// GH issue #557: when `true`, the daemon routes a `ReplaceInput`
    /// proposal from this extension's `on_input` hook through
    /// `AgentPause` so the user confirms the rewrite before it lands
    /// on the wire. Defaults to `false`.
    #[serde(default)]
    pub requires_user_approval: bool,
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("malformed manifest TOML: {0}")]
    MalformedToml(#[from] toml::de::Error),

    #[error("manifest field `{field}` is empty")]
    EmptyField { field: &'static str },

    #[error("manifest field `{field}` rejected: {reason}")]
    InvalidField { field: &'static str, reason: String },
}

impl ExtensionManifest {
    /// Parse an `extension.toml` body. Performs structural
    /// validation beyond what serde catches: rejects empty
    /// required fields and ill-formed ids.
    pub fn from_toml_str(raw: &str) -> Result<Self, ManifestError> {
        let parsed: ExtensionManifest = toml::from_str(raw)?;
        parsed.validate()?;
        Ok(parsed)
    }

    fn validate(&self) -> Result<(), ManifestError> {
        if self.id.trim().is_empty() {
            return Err(ManifestError::EmptyField { field: "id" });
        }
        if !valid_id(&self.id) {
            return Err(ManifestError::InvalidField {
                field: "id",
                reason: "expected lowercase letters, digits, `-`, or `_`".into(),
            });
        }
        if self.name.trim().is_empty() {
            return Err(ManifestError::EmptyField { field: "name" });
        }
        if self.version.trim().is_empty() {
            return Err(ManifestError::EmptyField { field: "version" });
        }
        match &self.entry {
            EntryKind::LocalScript { path } | EntryKind::NativeLibrary { path } => {
                if path.trim().is_empty() {
                    return Err(ManifestError::EmptyField {
                        field: "entry.path",
                    });
                }
            }
            EntryKind::Builtin => {}
        }
        Ok(())
    }
}

fn valid_id(id: &str) -> bool {
    id.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && !id.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_builtin_manifest() {
        let raw = r#"
id = "lint-helper"
name = "Lint Helper"
version = "0.1.0"

[entry]
kind = "builtin"
"#;
        let manifest = ExtensionManifest::from_toml_str(raw).unwrap();
        assert_eq!(manifest.id, "lint-helper");
        assert_eq!(manifest.name, "Lint Helper");
        assert_eq!(manifest.version, "0.1.0");
        assert!(matches!(manifest.entry, EntryKind::Builtin));
        assert!(manifest.capabilities.is_empty());
    }

    #[test]
    fn parses_local_script_with_capabilities_and_permissions() {
        let raw = r#"
id = "release-helper"
name = "Release Helper"
version = "0.2.1"
description = "drafts release notes"
capabilities = ["prompt_injection", "tool_provider"]
permissions = "read-only"
checksum = "sha256:abc123"
min_vulcan_version = "0.5.0"

[entry]
kind = "local_script"
path = "./run.sh"
"#;
        let m = ExtensionManifest::from_toml_str(raw).unwrap();
        assert_eq!(m.capabilities, vec!["prompt_injection", "tool_provider"]);
        assert_eq!(m.permissions.as_deref(), Some("read-only"));
        assert_eq!(m.checksum.as_deref(), Some("sha256:abc123"));
        assert_eq!(m.min_vulcan_version.as_deref(), Some("0.5.0"));
        match &m.entry {
            EntryKind::LocalScript { path } => assert_eq!(path, "./run.sh"),
            other => panic!("expected LocalScript, got {other:?}"),
        }
    }

    #[test]
    fn rejects_missing_required_id() {
        let raw = r#"
id = ""
name = "x"
version = "0.1.0"

[entry]
kind = "builtin"
"#;
        let err = ExtensionManifest::from_toml_str(raw).unwrap_err();
        assert!(matches!(err, ManifestError::EmptyField { field: "id" }));
    }

    #[test]
    fn rejects_invalid_id_characters() {
        let raw = r#"
id = "Lint Helper!"
name = "x"
version = "0.1.0"

[entry]
kind = "builtin"
"#;
        let err = ExtensionManifest::from_toml_str(raw).unwrap_err();
        match err {
            ManifestError::InvalidField { field, .. } => assert_eq!(field, "id"),
            other => panic!("expected InvalidField, got {other:?}"),
        }
    }

    #[test]
    fn rejects_local_script_with_empty_path() {
        let raw = r#"
id = "x"
name = "x"
version = "0.1.0"

[entry]
kind = "local_script"
path = ""
"#;
        let err = ExtensionManifest::from_toml_str(raw).unwrap_err();
        assert!(matches!(
            err,
            ManifestError::EmptyField {
                field: "entry.path"
            }
        ));
    }

    #[test]
    fn rejects_unknown_entry_kind() {
        let raw = r#"
id = "x"
name = "x"
version = "0.1.0"

[entry]
kind = "telepathy"
"#;
        let err = ExtensionManifest::from_toml_str(raw).unwrap_err();
        // Unknown variant comes from serde — falls into MalformedToml.
        assert!(matches!(err, ManifestError::MalformedToml(_)));
    }

    #[test]
    fn rejects_completely_malformed_toml() {
        let raw = "id = \nbroken [";
        let err = ExtensionManifest::from_toml_str(raw).unwrap_err();
        assert!(matches!(err, ManifestError::MalformedToml(_)));
    }

    #[test]
    fn round_trips_through_toml() {
        let raw = r#"
id = "lint-helper"
name = "Lint Helper"
version = "0.1.0"
description = "lint pass"

[entry]
kind = "builtin"
"#;
        let m = ExtensionManifest::from_toml_str(raw).unwrap();
        let serialized = toml::to_string(&m).unwrap();
        let parsed_back = ExtensionManifest::from_toml_str(&serialized).unwrap();
        assert_eq!(m, parsed_back);
    }

    #[test]
    fn requires_user_approval_defaults_to_false_and_parses_when_present() {
        let default_raw = r#"
id = "input-demo"
name = "Input Demo"
version = "0.1.0"
capabilities = ["input_interceptor"]

[entry]
kind = "builtin"
"#;
        let default_m = ExtensionManifest::from_toml_str(default_raw).unwrap();
        assert!(!default_m.requires_user_approval);

        let opted_in_raw = r#"
id = "input-demo"
name = "Input Demo"
version = "0.1.0"
capabilities = ["input_interceptor"]
requires_user_approval = true

[entry]
kind = "builtin"
"#;
        let opted_in_m = ExtensionManifest::from_toml_str(opted_in_raw).unwrap();
        assert!(opted_in_m.requires_user_approval);
    }
}
