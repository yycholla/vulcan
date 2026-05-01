//! YYC-165: extension foundation — typed metadata + registry.
//!
//! ## Scope of this PR (YYC-224)
//!
//! - `ExtensionMetadata` shape (id / name / version / description /
//!   source / status / permissions).
//! - `ExtensionCapability` enum — what an extension may contribute.
//! - `ExtensionRegistry` with deterministic load order +
//!   status reporting. Ordering uses (priority asc, id asc) so a
//!   tie-breaker on identical priorities is stable and testable.
//!
//! ## Deliberately deferred
//!
//! - DraftExtension parsing from skill frontmatter (YYC-165 PR-2).
//! - Config schema for `[extensions]` enable/disable (PR-3).
//! - Wiring code-backed extensions through `HookRegistry` (PR-4).
//! - Manifest `[config]` → `ConfigField` bridge for YYC-212 (PR-5).
//! - Dynamic loading (out of scope for the entire YYC-165 epic).

pub mod api;
pub mod audit;
pub mod config_field;
pub mod draft;
pub mod install_state;
pub mod manifest;
pub mod policy;
pub mod registry;
pub mod store;
pub mod verify;

pub use audit::{
    CompactionAuditAction, CompactionAuditEvent, ExtensionAuditEvent, ExtensionAuditLog,
    InputInterceptAction, InputInterceptEvent, PermissionAuditEvent, QuotaTracker,
};
pub use config_field::{ExtensionConfigField, ExtensionFieldKind};
pub use draft::parse_skill_extension;
pub use install_state::{InstallState, InstallStateStore, SqliteInstallStateStore};
pub use manifest::{EntryKind, ExtensionManifest, ManifestError};
pub use policy::{ExtensionPermission, ExtensionPolicyEngine, PolicyDecision};
pub use registry::{CodeExtension, ExtensionRegistry};
pub use store::{DiscoveredExtension, discover};
pub use verify::{VerificationError, verify_checksum_optional, verify_compatible};

use serde::{Deserialize, Serialize};

/// Lifecycle / activation state of an extension. Drives the
/// `vulcan extension list` view + tells the registry whether to
/// instantiate a code-backed extension's contributions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionStatus {
    /// Loaded into memory but not yet activated. Registries skip
    /// `Inactive` extensions when wiring hooks/tools.
    Inactive,
    /// Activated; capabilities are wired into the runtime.
    Active,
    /// Marked broken by the registry. Carries a `broken_reason`
    /// for diagnostics.
    Broken,
    /// Draft (markdown / metadata only). Even if loaded, no code
    /// runs — drafts are documentation candidates.
    Draft,
}

impl ExtensionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ExtensionStatus::Inactive => "inactive",
            ExtensionStatus::Active => "active",
            ExtensionStatus::Broken => "broken",
            ExtensionStatus::Draft => "draft",
        }
    }
}

/// Where this extension came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum ExtensionSource {
    /// Compiled into the vulcan binary (`built-in` registrations).
    Builtin,
    /// Read from `~/.vulcan/extensions/<id>/manifest.toml` —
    /// today this only carries metadata; code execution arrives
    /// once dynamic loading lands.
    LocalManifest,
    /// Read from `<workspace>/.vulcan/extensions/<id>/extension.toml`.
    /// Workspace-local manifests stay inactive until the user trusts
    /// the exact workspace/id/checksum tuple.
    UntrustedSource,
    /// Imported from a markdown skill (`<skills_dir>/<id>.md`)
    /// via the YYC-165 PR-2 promotion path.
    SkillDraft,
}

/// What an extension is allowed to contribute. Pure metadata for
/// now — registry uses this to show "what would activating this
/// do?" without instantiating anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionCapability {
    PromptInjection,
    HookHandler,
    ToolProvider,
    MemoryBackend,
    LifecycleObserver,
    /// GH issue #557: extension may register an `on_input` handler
    /// that blocks or rewrites raw user input. Activation refuses
    /// extensions that contribute an `on_input` hook without
    /// declaring this capability.
    InputInterceptor,
}

impl ExtensionCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            ExtensionCapability::PromptInjection => "prompt_injection",
            ExtensionCapability::HookHandler => "hook_handler",
            ExtensionCapability::ToolProvider => "tool_provider",
            ExtensionCapability::MemoryBackend => "memory_backend",
            ExtensionCapability::LifecycleObserver => "lifecycle_observer",
            ExtensionCapability::InputInterceptor => "input_interceptor",
        }
    }
}

/// Frontend-side capabilities a daemon extension may require before it
/// activates for a Session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendCapability {
    TextIo,
    RichText,
    CellCanvas,
    RawKeys,
    StatusWidgets,
    Tick30Hz,
    Tick60Hz,
}

impl FrontendCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            FrontendCapability::TextIo => "text_io",
            FrontendCapability::RichText => "rich_text",
            FrontendCapability::CellCanvas => "cell_canvas",
            FrontendCapability::RawKeys => "raw_keys",
            FrontendCapability::StatusWidgets => "status_widgets",
            FrontendCapability::Tick30Hz => "tick_30hz",
            FrontendCapability::Tick60Hz => "tick_60hz",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "text_io" => Some(Self::TextIo),
            "rich_text" => Some(Self::RichText),
            "cell_canvas" => Some(Self::CellCanvas),
            "raw_input" | "raw_keys" => Some(Self::RawKeys),
            "status_widgets" => Some(Self::StatusWidgets),
            "tick_30hz" => Some(Self::Tick30Hz),
            "tick_60hz" => Some(Self::Tick60Hz),
            _ => None,
        }
    }

    pub fn text_only() -> Vec<Self> {
        vec![Self::TextIo]
    }

    pub fn full_set() -> Vec<Self> {
        vec![
            Self::TextIo,
            Self::RichText,
            Self::CellCanvas,
            Self::RawKeys,
            Self::StatusWidgets,
            Self::Tick30Hz,
            Self::Tick60Hz,
        ]
    }
}

/// Static description of an extension. Carries everything needed
/// to render `vulcan extension list/show` and to decide whether
/// to activate, without touching code execution paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionMetadata {
    pub id: String,
    pub name: String,
    /// Free-form version string. Not parsed; conventional but not
    /// validated against semver here.
    pub version: String,
    pub description: String,
    pub source: ExtensionSource,
    pub status: ExtensionStatus,
    /// Capabilities this extension declares it would contribute
    /// when active. Display-only today; PR-4 enforces against the
    /// active hook/tool wiring.
    pub capabilities: Vec<ExtensionCapability>,
    /// Frontend capabilities required before this extension should be
    /// activated for a frontend-backed Session.
    #[serde(default)]
    pub requires_frontend: Vec<FrontendCapability>,
    /// Optional human-readable permissions summary surfaced in
    /// `vulcan extension show`. Caller-provided; the registry
    /// does not interpret it.
    pub permissions_summary: Option<String>,
    /// Whether input rewrites from this extension require explicit
    /// user approval before the rewritten text is applied.
    pub requires_user_approval: bool,
    /// When `status == Broken`, free-form explanation. `None` for
    /// any other status.
    pub broken_reason: Option<String>,
    /// Load priority. Lower numbers load first. Tied priorities
    /// fall back to id-asc so ordering stays deterministic.
    pub priority: i32,
}

impl ExtensionMetadata {
    /// Builder convenience: minimal metadata with sensible
    /// defaults. Most call sites just want id + name + version +
    /// source.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        version: impl Into<String>,
        source: ExtensionSource,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            version: version.into(),
            description: String::new(),
            source,
            status: ExtensionStatus::Inactive,
            capabilities: Vec::new(),
            requires_frontend: Vec::new(),
            permissions_summary: None,
            requires_user_approval: false,
            broken_reason: None,
            priority: 100,
        }
    }
}
