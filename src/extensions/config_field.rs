//! YYC-228 (YYC-165 PR-5): extension-side `ConfigField`.
//!
//! Mirrors YYC-212's `ConfigField` shape but uses owned `String`
//! rather than `&'static str` so extensions can declare fields
//! at runtime. The YYC-212 CLI reads from both surfaces (built-
//! in static catalog + extension-contributed dynamic catalog)
//! when listing / editing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ExtensionFieldKind {
    Bool,
    Int { min: Option<i64>, max: Option<i64> },
    Float { min: Option<f64>, max: Option<f64> },
    Enum { variants: Vec<String> },
    String { secret: bool },
    Path,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionConfigField {
    /// Dotted key path *under* the owning extension's id, e.g.
    /// `lint-helper.threshold`. Renderers prefix the extension id
    /// when displaying.
    pub path: String,
    pub kind: ExtensionFieldKind,
    /// Display string for the default value.
    pub default: String,
    pub help: String,
}

impl ExtensionConfigField {
    pub fn bool_field(path: impl Into<String>, default: bool, help: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind: ExtensionFieldKind::Bool,
            default: default.to_string(),
            help: help.into(),
        }
    }

    pub fn enum_field(
        path: impl Into<String>,
        variants: Vec<String>,
        default: impl Into<String>,
        help: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            kind: ExtensionFieldKind::Enum { variants },
            default: default.into(),
            help: help.into(),
        }
    }

    pub fn string_field(
        path: impl Into<String>,
        secret: bool,
        default: impl Into<String>,
        help: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            kind: ExtensionFieldKind::String { secret },
            default: default.into(),
            help: help.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_field_round_trips_through_serde_json() {
        let f = ExtensionConfigField::bool_field("foo.enabled", true, "toggle the thing");
        let json = serde_json::to_string(&f).unwrap();
        let back: ExtensionConfigField = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn enum_field_carries_variants() {
        let f = ExtensionConfigField::enum_field(
            "lint.mode",
            vec!["off".into(), "warn".into(), "block".into()],
            "warn",
            "Aggressiveness of the linter pass.",
        );
        match &f.kind {
            ExtensionFieldKind::Enum { variants } => {
                assert_eq!(variants, &["off", "warn", "block"]);
            }
            other => panic!("expected Enum, got {other:?}"),
        }
    }
}
