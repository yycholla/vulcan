//! Cargo-crate extension API surface (Slice 1 / GH issue #549).
//!
//! Splits today's `CodeExtension` trait into two roles aligned with
//! `src/extensions/CONTEXT.md`:
//!
//! - **`DaemonCodeExtension`** — daemon-global registration that
//!   instantiates per-**Session** state. Registered once at daemon
//!   startup via `inventory::submit!`.
//! - **`SessionExtension`** — per-**Session** instantiation owning that
//!   session's hooks, tools, commands, providers, and lifecycle handlers.
//!
//! The existing `CodeExtension` trait in `super::registry` stays
//! alongside this module while migration is in flight; new extensions
//! target this API.

use std::path::PathBuf;
use std::sync::Arc;

use super::{ExtensionMetadata, ExtensionStateContext, FrontendCapability};
use crate::hooks::HookHandler;
use crate::memory::SessionStore;
use crate::provider::LLMProvider;
use crate::provider::factory::ProviderFactory;
use crate::tools::Tool;
use serde_json::Value;
use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub struct FrontendEvent {
    pub session_id: String,
    pub extension_id: String,
    pub payload: Value,
}

#[derive(Clone)]
pub struct FrontendEventSink {
    tx: Option<broadcast::Sender<FrontendEvent>>,
}

impl FrontendEventSink {
    pub fn new(tx: broadcast::Sender<FrontendEvent>) -> Self {
        Self { tx: Some(tx) }
    }

    pub fn noop() -> Self {
        Self { tx: None }
    }

    pub fn emit(&self, event: FrontendEvent) -> anyhow::Result<()> {
        if let Some(tx) = &self.tx {
            let _ = tx.send(event);
        }
        Ok(())
    }
}

/// Parsed `[package.metadata.vulcan]` block for a cargo-crate
/// extension. Produced by `vulcan_extension_macros::include_manifest!()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionManifest {
    pub id: String,
    pub version: String,
    pub daemon_entry: Option<String>,
    pub requires: Vec<String>,
    pub requires_user_approval: bool,
    pub provider_defaults: Option<ExtensionProviderDefaults>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExtensionProviderDefaults {
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub timeout_ms: Option<u64>,
}

/// Per-**Session** instantiation context handed to a
/// `DaemonCodeExtension::instantiate` call. Carries the bare minimum
/// the auto-commit dogfood needs today; grows toward the full
/// `ExtensionContext` (model, provider, pause, state, ui, ...) as
/// later slices land.
#[derive(Clone)]
pub struct SessionExtensionCtx {
    /// Working directory of the **Session** the extension is being
    /// instantiated for. Auto-commit reads this to know which repo to
    /// `git commit` against.
    pub cwd: PathBuf,
    /// **Session** identifier. Routes telemetry, audit log entries,
    /// and per-session state under a stable key.
    pub session_id: String,
    /// Durable Session History store for replaying extension state from
    /// prior tool results during `session_start`.
    pub memory: Arc<SessionStore>,
    /// Capabilities declared by the connected frontend or gateway lane
    /// for this Session.
    pub frontend_capabilities: Vec<FrontendCapability>,
    /// Frontend modules declared by the connected client. Daemon
    /// activation compares matching ids against daemon module metadata
    /// to detect split-crate version skew.
    pub frontend_extensions: Vec<vulcan_frontend_api::FrontendExtensionDescriptor>,
    /// Extension-local state handle scoped to this Session and
    /// extension id.
    pub state: ExtensionStateContext,
    /// Cross-side daemon-to-frontend event channel.
    pub frontend_events: FrontendEventSink,
}

impl SessionExtensionCtx {
    pub fn with_extension(
        mut self,
        extension_id: &str,
        capabilities: Vec<super::ExtensionCapability>,
    ) -> Self {
        self.state = self.state.for_extension(extension_id, capabilities);
        self
    }

    pub fn emit_frontend_event(&self, payload: Value) -> anyhow::Result<()> {
        self.frontend_events.emit(FrontendEvent {
            session_id: self.session_id.clone(),
            extension_id: self.state.extension_id().to_string(),
            payload,
        })
    }
}

pub type ExtensionContext = SessionExtensionCtx;

/// Daemon-global registration for an extension. One implementation per
/// installed extension crate that ships a daemon module; registered
/// once at daemon startup, instantiated per-**Session**.
pub trait DaemonCodeExtension: Send + Sync {
    /// Static metadata describing this extension. Must match the
    /// metadata under which the registry indexes the extension.
    fn metadata(&self) -> ExtensionMetadata;

    /// Instantiate per-**Session** state. Called once per Session at
    /// construction with the session's context. Returns an
    /// `Arc<dyn SessionExtension>` that owns hooks, tools, commands,
    /// providers, and lifecycle handlers for that session.
    fn instantiate(&self, ctx: SessionExtensionCtx) -> Arc<dyn SessionExtension>;
}

/// Extension-contributed command surface placeholder. Slice 1 only
/// needs the typed registration slot; concrete daemon/frontend command
/// routing lands in later slices.
pub trait ExtensionCommand: Send + Sync {}

/// Per-**Session** instantiation of a `DaemonCodeExtension`. Owns
/// hooks, tools, commands, providers, provider factories, and lifecycle
/// handlers.
///
/// All methods default-empty so an extension can opt into only the
/// surfaces it needs.
#[async_trait::async_trait]
pub trait SessionExtension: Send + Sync {
    /// Hook handlers this **Session Extension** contributes. Wired
    /// into the session's `HookRegistry` once at session construction.
    /// Default returns nothing — extensions that don't observe hook
    /// events leave this unimplemented.
    fn hook_handlers(&self) -> Vec<Arc<dyn HookHandler>> {
        Vec::new()
    }

    /// Tools this session extension contributes.
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    /// Commands this session extension contributes.
    fn commands(&self) -> Vec<Arc<dyn ExtensionCommand>> {
        Vec::new()
    }

    /// Concrete provider instances this session extension contributes.
    fn providers(&self) -> Vec<(String, Arc<dyn LLMProvider>)> {
        Vec::new()
    }

    /// Provider factories this session extension contributes.
    fn provider_factories(&self) -> Vec<(String, Arc<dyn ProviderFactory>)> {
        Vec::new()
    }

    /// Lifecycle hook invoked when the session extension is activated.
    async fn on_activate(&self) {}

    /// Lifecycle hook invoked when the session extension is deactivated.
    async fn on_deactivate(&self) {}
}

/// Inventory-collected registration entry. Each extension crate
/// contributes one `ExtensionRegistration` via `inventory::submit!` at
/// the call site. The daemon iterates `inventory::iter` at startup and
/// calls `register` to materialize the trait object.
pub struct ExtensionRegistration {
    /// Function pointer that constructs the daemon-side extension.
    /// Called once at daemon startup.
    pub register: fn() -> Arc<dyn DaemonCodeExtension>,
}

inventory::collect!(ExtensionRegistration);

/// Collect every `inventory::submit!`-registered extension. Returned
/// in source-order (`inventory` does not guarantee a sort); callers
/// that need deterministic ordering should sort by `metadata().id`.
pub fn collect_registrations() -> Vec<Arc<dyn DaemonCodeExtension>> {
    inventory::iter::<ExtensionRegistration>()
        .map(|entry| (entry.register)())
        .collect()
}

/// Wire every `inventory::submit!`-registered cargo-crate extension
/// into the supplied [`ExtensionRegistry`]. Called once at daemon
/// startup. Returns the number of extensions registered so the caller
/// can log it.
pub fn wire_inventory_into_registry(registry: &super::ExtensionRegistry) -> usize {
    let mut count = 0usize;
    for ext in collect_registrations() {
        registry.register_daemon_extension(ext);
        count += 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::ExtensionSource;
    use crate::hooks::HookRegistry;

    struct StubExtension;

    impl DaemonCodeExtension for StubExtension {
        fn metadata(&self) -> ExtensionMetadata {
            ExtensionMetadata::new(
                "stub-inventory",
                "Stub Inventory Extension",
                "0.0.1",
                ExtensionSource::Builtin,
            )
        }
        fn instantiate(&self, _ctx: SessionExtensionCtx) -> Arc<dyn SessionExtension> {
            struct StubSession;
            impl SessionExtension for StubSession {}
            Arc::new(StubSession)
        }
    }

    /// Test-only ctx with deterministic placeholder values.
    fn test_ctx() -> SessionExtensionCtx {
        SessionExtensionCtx {
            cwd: PathBuf::from("/tmp/test-session"),
            session_id: "test-session-id".to_string(),
            memory: Arc::new(SessionStore::in_memory()),
            frontend_capabilities: FrontendCapability::full_set(),
            frontend_extensions: Vec::new(),
            state: ExtensionStateContext::in_memory_for_tests("test-session-id", "test"),
            frontend_events: FrontendEventSink::noop(),
        }
    }

    inventory::submit! {
        ExtensionRegistration {
            register: || Arc::new(StubExtension) as Arc<dyn DaemonCodeExtension>,
        }
    }

    #[test]
    fn inventory_registered_extension_appears_in_collection() {
        let registrations = collect_registrations();
        let ids: Vec<String> = registrations.iter().map(|ext| ext.metadata().id).collect();
        assert!(
            ids.contains(&"stub-inventory".to_string()),
            "expected stub-inventory in {ids:?}"
        );
    }

    #[test]
    fn registry_register_daemon_extension_surfaces_metadata_in_list() {
        use crate::extensions::ExtensionRegistry;

        let registry = ExtensionRegistry::new();
        let ext: Arc<dyn DaemonCodeExtension> = Arc::new(StubExtension);
        registry.register_daemon_extension(ext);

        let ids: Vec<String> = registry.list().into_iter().map(|m| m.id).collect();
        assert!(
            ids.contains(&"stub-inventory".to_string()),
            "expected stub-inventory in registry list, got {ids:?}"
        );
    }

    #[test]
    fn wire_inventory_populates_registry_with_every_registered_extension() {
        use crate::extensions::ExtensionRegistry;

        let registry = ExtensionRegistry::new();
        let registered = wire_inventory_into_registry(&registry);

        assert!(
            registered >= 1,
            "expected wire_inventory to register at least one extension, got {registered}"
        );
        let ids: Vec<String> = registry.list().into_iter().map(|m| m.id).collect();
        assert!(
            ids.contains(&"stub-inventory".to_string()),
            "expected stub-inventory in registry list, got {ids:?}"
        );
    }

    #[test]
    fn wire_daemon_extensions_instantiates_and_registers_hook_handlers_per_session() {
        use crate::extensions::ExtensionRegistry;
        use crate::hooks::{HookHandler, HookRegistry};

        struct WatcherSession;
        struct WatcherHook;

        #[async_trait::async_trait]
        impl HookHandler for WatcherHook {
            fn name(&self) -> &str {
                "watcher-hook"
            }
        }

        impl SessionExtension for WatcherSession {
            fn hook_handlers(&self) -> Vec<Arc<dyn HookHandler>> {
                vec![Arc::new(WatcherHook)]
            }
        }

        struct WatcherExtension;
        impl DaemonCodeExtension for WatcherExtension {
            fn metadata(&self) -> ExtensionMetadata {
                let mut m = ExtensionMetadata::new(
                    "watcher-ext",
                    "Watcher",
                    "0.0.1",
                    ExtensionSource::Builtin,
                );
                m.status = crate::extensions::ExtensionStatus::Active;
                m
            }
            fn instantiate(&self, _ctx: SessionExtensionCtx) -> Arc<dyn SessionExtension> {
                Arc::new(WatcherSession)
            }
        }

        let registry = ExtensionRegistry::new();
        registry.register_daemon_extension(Arc::new(WatcherExtension));

        let mut hooks = HookRegistry::new();
        let registered = registry.wire_daemon_extensions(test_ctx(), &mut hooks);

        assert_eq!(registered, 1);
        assert_eq!(hooks.handler_count(), 1);
    }

    #[tokio::test]
    async fn wire_daemon_extensions_marks_input_rewrite_approval_requirement() {
        use crate::extensions::ExtensionRegistry;
        use crate::hooks::{HookHandler, HookOutcome, HookRegistry, InputDecision};
        use tokio_util::sync::CancellationToken;

        struct RewriterSession;
        struct RewriterHook;

        #[async_trait::async_trait]
        impl HookHandler for RewriterHook {
            fn name(&self) -> &str {
                "approval-ext"
            }
            async fn on_input(
                &self,
                _raw: &str,
                _cancel: CancellationToken,
            ) -> anyhow::Result<HookOutcome> {
                Ok(HookOutcome::ReplaceInput("rewritten".into()))
            }
        }

        impl SessionExtension for RewriterSession {
            fn hook_handlers(&self) -> Vec<Arc<dyn HookHandler>> {
                vec![Arc::new(RewriterHook)]
            }
        }

        struct RewriterExtension;
        impl DaemonCodeExtension for RewriterExtension {
            fn metadata(&self) -> ExtensionMetadata {
                let mut m = ExtensionMetadata::new(
                    "approval-ext",
                    "Approval Ext",
                    "0.0.1",
                    ExtensionSource::Builtin,
                );
                m.status = crate::extensions::ExtensionStatus::Active;
                m.requires_user_approval = true;
                m
            }
            fn instantiate(&self, _ctx: SessionExtensionCtx) -> Arc<dyn SessionExtension> {
                Arc::new(RewriterSession)
            }
        }

        let registry = ExtensionRegistry::new();
        registry.register_daemon_extension(Arc::new(RewriterExtension));

        let mut hooks = HookRegistry::new();
        let registered = registry.wire_daemon_extensions(test_ctx(), &mut hooks);
        assert_eq!(registered, 1);

        match hooks.apply_on_input("raw", CancellationToken::new()).await {
            InputDecision::Block(reason) => assert!(reason.contains("no pause channel")),
            other => panic!("expected approval block, got {other:?}"),
        }
    }

    #[test]
    fn wire_daemon_extensions_registers_session_tools_with_prefix() {
        use crate::extensions::ExtensionRegistry;
        use crate::tools::{Tool, ToolRegistry, ToolResult};
        use serde_json::json;
        use tokio_util::sync::CancellationToken;

        struct LocalTool;
        #[async_trait::async_trait]
        impl Tool for LocalTool {
            fn name(&self) -> &str {
                "ping"
            }
            fn description(&self) -> &str {
                "Ping"
            }
            fn schema(&self) -> serde_json::Value {
                json!({ "type": "object", "properties": {} })
            }
            async fn call(
                &self,
                _params: serde_json::Value,
                _cancel: CancellationToken,
                _progress: Option<crate::tools::ProgressSink>,
            ) -> anyhow::Result<ToolResult> {
                Ok(ToolResult::ok("pong"))
            }
        }

        struct ToolSession;
        impl SessionExtension for ToolSession {
            fn tools(&self) -> Vec<Arc<dyn Tool>> {
                vec![Arc::new(LocalTool)]
            }
        }

        struct ToolExtension;
        impl DaemonCodeExtension for ToolExtension {
            fn metadata(&self) -> ExtensionMetadata {
                let mut m = ExtensionMetadata::new(
                    "tool-ext",
                    "Tool Ext",
                    "0.0.1",
                    ExtensionSource::Builtin,
                );
                m.status = crate::extensions::ExtensionStatus::Active;
                m
            }
            fn instantiate(&self, _ctx: SessionExtensionCtx) -> Arc<dyn SessionExtension> {
                Arc::new(ToolSession)
            }
        }

        let registry = ExtensionRegistry::new();
        registry.register_daemon_extension(Arc::new(ToolExtension));
        let mut hooks = HookRegistry::new();
        let mut tools = ToolRegistry::new();

        let (sessions, extension_tools) =
            registry.wire_daemon_extensions_into_runtime(test_ctx(), &mut hooks, Some(&mut tools));

        assert_eq!(sessions, 1);
        assert_eq!(extension_tools, 1);
        assert!(tools.contains("tool-ext_ping"));
        assert!(!tools.contains("ping"));
    }

    #[test]
    fn session_extension_ctx_carries_cwd_and_session_id_to_instantiate() {
        use parking_lot::RwLock;
        use std::path::PathBuf;

        struct CapturingExtension {
            seen: Arc<RwLock<Option<(PathBuf, String)>>>,
        }
        impl DaemonCodeExtension for CapturingExtension {
            fn metadata(&self) -> ExtensionMetadata {
                ExtensionMetadata::new("capturing", "Capturing", "0.0.1", ExtensionSource::Builtin)
            }
            fn instantiate(&self, ctx: SessionExtensionCtx) -> Arc<dyn SessionExtension> {
                struct Noop;
                impl SessionExtension for Noop {}
                *self.seen.write() = Some((ctx.cwd, ctx.session_id));
                Arc::new(Noop)
            }
        }

        let seen = Arc::new(RwLock::new(None));
        let ext: Arc<dyn DaemonCodeExtension> = Arc::new(CapturingExtension { seen: seen.clone() });
        let ctx = SessionExtensionCtx {
            cwd: PathBuf::from("/tmp/example-session"),
            session_id: "sess-42".to_string(),
            memory: Arc::new(SessionStore::in_memory()),
            frontend_capabilities: FrontendCapability::full_set(),
            frontend_extensions: Vec::new(),
            state: ExtensionStateContext::in_memory_for_tests("sess-42", "capturing"),
            frontend_events: FrontendEventSink::noop(),
        };
        let _ = ext.instantiate(ctx);

        let captured = seen.read().clone().expect("instantiate ran");
        assert_eq!(captured.0, PathBuf::from("/tmp/example-session"));
        assert_eq!(captured.1, "sess-42");
    }

    #[test]
    fn extension_context_emits_frontend_event_envelope() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(4);
        let ctx = SessionExtensionCtx {
            frontend_events: FrontendEventSink::new(tx),
            ..test_ctx().with_extension("stub-inventory", Vec::new())
        };

        ctx.emit_frontend_event(serde_json::json!({ "hello": "frontend" }))
            .expect("emit succeeds");

        let event = rx.try_recv().expect("frontend event");
        assert_eq!(event.session_id, "test-session-id");
        assert_eq!(event.extension_id, "stub-inventory");
        assert_eq!(event.payload["hello"], "frontend");
    }

    #[test]
    fn instantiated_session_extension_exposes_its_hook_handlers() {
        use crate::hooks::HookHandler;

        struct WatcherSession;
        struct WatcherHook;

        #[async_trait::async_trait]
        impl HookHandler for WatcherHook {
            fn name(&self) -> &str {
                "watcher"
            }
        }

        impl SessionExtension for WatcherSession {
            fn hook_handlers(&self) -> Vec<Arc<dyn HookHandler>> {
                vec![Arc::new(WatcherHook)]
            }
        }

        struct WatcherExtension;
        impl DaemonCodeExtension for WatcherExtension {
            fn metadata(&self) -> ExtensionMetadata {
                ExtensionMetadata::new("watcher", "Watcher", "0.0.1", ExtensionSource::Builtin)
            }
            fn instantiate(&self, _ctx: SessionExtensionCtx) -> Arc<dyn SessionExtension> {
                Arc::new(WatcherSession)
            }
        }

        let daemon_ext: Arc<dyn DaemonCodeExtension> = Arc::new(WatcherExtension);
        let session_ext = daemon_ext.instantiate(test_ctx());
        let handlers = session_ext.hook_handlers();
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].name(), "watcher");
    }

    #[tokio::test]
    async fn session_extension_default_surface_is_empty_and_noop() {
        struct MinimalSession;
        impl SessionExtension for MinimalSession {}

        let session = MinimalSession;
        assert!(session.hook_handlers().is_empty());
        assert!(session.tools().is_empty());
        assert!(session.commands().is_empty());
        assert!(session.providers().is_empty());
        assert!(session.provider_factories().is_empty());
        session.on_activate().await;
        session.on_deactivate().await;
    }
}
