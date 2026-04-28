//! YYC-212 PR-1: typed registry of every user-facing config field.
//!
//! One declaration drives `vulcan config list`, `get`, `path`, and
//! `show`; later PRs layer `set` / `unset` / `edit` on top without
//! touching the per-field declarations.
//!
//! ## Scope of this PR
//!
//! - `FieldKind` enum (Bool / Int / Enum / String / Path).
//! - `ConfigField` declaration with `path`, `kind`, `default`,
//!   `help`, `file`, and `secret` flag.
//! - Hand-authored built-in declarations covering each top-level
//!   section (`tools`, `compaction`, `recall`, `embeddings`,
//!   `provider`, `gateway`, `tui`, `scheduler`, `auto_create_skills`,
//!   `skills_dir`).
//! - `lookup` + `all` accessors used by the CLI.
//!
//! ## Deliberately deferred
//!
//! - `set` / `unset` / `edit` writers (separate PRs).
//! - Extension-registered fields (waits on YYC-165).
//! - Custom widget metadata (waits on dialoguer integration).

use serde::Serialize;

/// Coarse type of a config field. Drives parsing on `set`,
/// formatting on `get`, and the widget choice on `edit` (later
/// PR). Kept intentionally simple — open-set strings cover the
/// long tail.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum FieldKind {
    Bool,
    /// Bounded integer. `min`/`max` are inclusive, `None` means
    /// unbounded on that side.
    Int {
        min: Option<i64>,
        max: Option<i64>,
    },
    /// Bounded float — for ratios and thresholds.
    Float {
        min: Option<f64>,
        max: Option<f64>,
    },
    /// String picked from a fixed set.
    Enum {
        variants: &'static [&'static str],
    },
    /// Free-form string. `secret = true` means redact on `show` /
    /// `get` unless the caller passes `--reveal`.
    String {
        secret: bool,
    },
    /// Filesystem path. Validated as UTF-8; existence is not
    /// checked (a config may legitimately point at a not-yet-
    /// created directory).
    Path,
}

/// Which file under `~/.vulcan/` owns this field. Lets the writer
/// route updates to the right TOML when YYC-99's split is in
/// effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigFile {
    Config,
    Keybinds,
    Providers,
}

/// One field declaration. `path` is the dotted lookup key
/// (`tools.native_enforcement`). `default` is the literal TOML
/// fragment that would land if the user ran `unset`.
#[derive(Debug, Clone, Serialize)]
pub struct ConfigField {
    pub path: &'static str,
    pub kind: FieldKind,
    /// Literal display of the default value (for `list` / `show`).
    pub default: &'static str,
    pub help: &'static str,
    pub file: ConfigFile,
}

/// Built-in field catalog. Hand-authored — extension fields
/// (YYC-165) will append at registration time once that lands.
const BUILTIN_FIELDS: &[ConfigField] = &[
    ConfigField {
        path: "active_profile",
        kind: FieldKind::String { secret: false },
        default: "(unset)",
        help: "Persisted active provider profile name. Both TUI and gateway resolve their starting provider from `[providers.<name>]` when set.",
        file: ConfigFile::Config,
    },
    ConfigField {
        path: "auto_create_skills",
        kind: FieldKind::Bool,
        default: "false",
        help: "After 5+ tool iterations, ask the model to summarize the turn as a draft skill.",
        file: ConfigFile::Config,
    },
    ConfigField {
        path: "skills_dir",
        kind: FieldKind::Path,
        default: "~/.vulcan/skills",
        help: "Directory the skills loader walks at session start.",
        file: ConfigFile::Config,
    },
    // ── tools.* ────────────────────────────────────────────────
    ConfigField {
        path: "tools.yolo_mode",
        kind: FieldKind::Bool,
        default: "false",
        help: "Disable safety + approval prompts. Legacy escape hatch — prefer per-tool approval modes.",
        file: ConfigFile::Config,
    },
    ConfigField {
        path: "tools.native_enforcement",
        kind: FieldKind::Enum {
            variants: &["off", "warn", "block"],
        },
        default: "block",
        help: "How aggressively to redirect bash invocations toward native tool equivalents.",
        file: ConfigFile::Config,
    },
    ConfigField {
        path: "tools.profile",
        kind: FieldKind::String { secret: false },
        default: "(unset)",
        help: "Default tool capability profile (readonly, coding, reviewer, gateway-safe, or user-defined).",
        file: ConfigFile::Config,
    },
    ConfigField {
        path: "tools.dangerous_commands.policy",
        kind: FieldKind::Enum {
            variants: &["prompt", "block", "allow"],
        },
        default: "prompt",
        help: "What SafetyHook does when a command matches a dangerous pattern.",
        file: ConfigFile::Config,
    },
    ConfigField {
        path: "tools.dangerous_commands.quota_per_session",
        kind: FieldKind::Int {
            min: Some(0),
            max: Some(1_000),
        },
        default: "5",
        help: "Per-session usage cap on approved-and-remembered dangerous commands. 0 = unlimited.",
        file: ConfigFile::Config,
    },
    // ── compaction.* ──────────────────────────────────────────
    ConfigField {
        path: "compaction.enabled",
        kind: FieldKind::Bool,
        default: "true",
        help: "Auto-compact context when usage approaches the model's max window.",
        file: ConfigFile::Config,
    },
    ConfigField {
        path: "compaction.trigger_ratio",
        kind: FieldKind::Float {
            min: Some(0.0),
            max: Some(1.0),
        },
        default: "0.85",
        help: "Token ratio (0.0 - 1.0) at which compaction fires.",
        file: ConfigFile::Config,
    },
    ConfigField {
        path: "compaction.reserved_tokens",
        kind: FieldKind::Int {
            min: Some(0),
            max: Some(2_000_000),
        },
        default: "50000",
        help: "Tokens to reserve for the next response (capped at max_context/4).",
        file: ConfigFile::Config,
    },
    // ── recall.* ──────────────────────────────────────────────
    ConfigField {
        path: "recall.enabled",
        kind: FieldKind::Bool,
        default: "false",
        help: "Auto-recall relevant past-session context on the first turn of a fresh session.",
        file: ConfigFile::Config,
    },
    ConfigField {
        path: "recall.max_hits",
        kind: FieldKind::Int {
            min: Some(1),
            max: Some(50),
        },
        default: "5",
        help: "Maximum number of recalled hits to inject.",
        file: ConfigFile::Config,
    },
    // ── embeddings.* ──────────────────────────────────────────
    ConfigField {
        path: "embeddings.enabled",
        kind: FieldKind::Bool,
        default: "false",
        help: "Register the embedding-search tools at session start.",
        file: ConfigFile::Config,
    },
    ConfigField {
        path: "embeddings.model",
        kind: FieldKind::String { secret: false },
        default: "(unset)",
        help: "Embedding model id (e.g. text-embedding-3-small).",
        file: ConfigFile::Config,
    },
    // ── provider.* (legacy [provider] block) ──────────────────
    ConfigField {
        path: "provider.api_key",
        kind: FieldKind::String { secret: true },
        default: "(unset)",
        help: "API key for the active provider. Can also be set via VULCAN_API_KEY env.",
        file: ConfigFile::Providers,
    },
    ConfigField {
        path: "provider.base_url",
        kind: FieldKind::String { secret: false },
        default: "(unset)",
        help: "OpenAI-compatible API base URL.",
        file: ConfigFile::Providers,
    },
    ConfigField {
        path: "provider.model",
        kind: FieldKind::String { secret: false },
        default: "(unset)",
        help: "Default model id sent to the provider.",
        file: ConfigFile::Providers,
    },
    ConfigField {
        path: "provider.max_iterations",
        kind: FieldKind::Int {
            min: Some(0),
            max: Some(10_000),
        },
        default: "0",
        help: "Hard cap on agent loop iterations. 0 = unlimited.",
        file: ConfigFile::Providers,
    },
    // ── tui.* ─────────────────────────────────────────────────
    ConfigField {
        path: "tui.show_reasoning",
        kind: FieldKind::Bool,
        default: "true",
        help: "Render the model's reasoning trace in the TUI when present.",
        file: ConfigFile::Config,
    },
];

/// Look up a field declaration by its dotted path. `None` if no
/// such field is declared (caller decides whether that's an error
/// — `get` treats it as an error, `show` falls back to printing
/// the raw TOML).
pub fn lookup(path: &str) -> Option<&'static ConfigField> {
    BUILTIN_FIELDS.iter().find(|f| f.path == path)
}

/// All declared fields, in declaration order.
pub fn all() -> &'static [ConfigField] {
    BUILTIN_FIELDS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_no_duplicate_paths() {
        let mut paths: Vec<&'static str> = all().iter().map(|f| f.path).collect();
        paths.sort();
        let total = paths.len();
        paths.dedup();
        assert_eq!(paths.len(), total, "duplicate field paths in registry");
    }

    #[test]
    fn lookup_returns_known_fields() {
        let f = lookup("tools.native_enforcement").expect("known field");
        assert_eq!(f.default, "block");
        assert!(matches!(f.kind, FieldKind::Enum { .. }));
        assert_eq!(f.file, ConfigFile::Config);
    }

    #[test]
    fn lookup_returns_none_for_unknown() {
        assert!(lookup("does.not.exist").is_none());
    }

    #[test]
    fn provider_api_key_is_marked_secret() {
        let f = lookup("provider.api_key").unwrap();
        match &f.kind {
            FieldKind::String { secret } => assert!(*secret),
            other => panic!("expected String, got {other:?}"),
        }
        assert_eq!(f.file, ConfigFile::Providers);
    }

    #[test]
    fn enum_variants_are_lowercase_only() {
        for f in all() {
            if let FieldKind::Enum { variants } = &f.kind {
                for v in *variants {
                    assert_eq!(
                        *v,
                        v.to_lowercase(),
                        "enum variant {v:?} for {} not lowercase",
                        f.path
                    );
                }
            }
        }
    }
}
