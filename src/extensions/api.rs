//! Cargo-crate extension API surface (Slice 1 / GH issue #549).
//!
//! Forward daemon/frontend extension API aligned with
//! `src/extensions/CONTEXT.md`:
//!
//! - **`DaemonCodeExtension`** — daemon-global registration that
//!   instantiates per-**Session** state. Registered once at daemon
//!   startup via `inventory::submit!`.
//! - **`SessionExtension`** — per-**Session** instantiation owning that
//!   session's hooks, tools, commands, providers, and lifecycle handlers.
//!
//! New daemon-side extensions target this API.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::ExtensionConfigField;
use super::ExtensionMetadata;
use super::FrontendCapability;
use super::state::{ExtensionStateScope, ExtensionStateStore};
use crate::config::DangerousCommandsConfig;
use crate::hooks::HookHandler;
use crate::memory::SessionStore;
use crate::orchestration::OrchestrationHook;
use crate::pause::PauseSender;
use crate::provider::LLMProvider;
use crate::provider::factory::ProviderFactory;
use crate::tools::{Tool, ToolResult};
use anyhow::Result;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Parsed `[package.metadata.vulcan]` block for a cargo-crate
/// extension. Produced by `vulcan_extension_macros::include_manifest!()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionManifest {
    pub id: String,
    pub version: String,
    pub daemon_entry: Option<String>,
    pub core: bool,
    pub requires_user_approval: bool,
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
    /// Durable session history store for replaying extension state
    /// carried through `ToolResult.details`.
    pub memory: Arc<SessionStore>,
    /// Skills directory configured for this Session. Core skills uses
    /// this to build the same catalog the previous hard-wired hook used.
    pub skills_dir: PathBuf,
    /// Optional interactive pause channel for Session extensions that
    /// need HITL approval, including core safety.
    pub pause_tx: Option<PauseSender>,
    /// Dangerous-command policy for core safety.
    pub dangerous_commands: DangerousCommandsConfig,
    /// Capabilities declared by the current frontend.
    pub frontend_capabilities: Vec<FrontendCapability>,
    /// Frontend extension descriptors declared by the current frontend.
    pub frontend_extensions: Vec<vulcan_frontend_api::FrontendExtensionDescriptor>,
    /// Push channel for daemon extensions to emit frontend events.
    pub frontend_events: FrontendEventSink,
    /// Durable extension-owned state store. Extensions access it through
    /// `state()` so all operations are scoped to their own extension id.
    pub extension_state: Option<Arc<dyn ExtensionStateStore>>,
    /// The daemon extension id this context is currently scoped to.
    pub extension_id: Option<String>,
}

impl SessionExtensionCtx {
    pub fn new(cwd: PathBuf, session_id: String, memory: Arc<SessionStore>) -> Self {
        Self {
            cwd,
            session_id,
            memory,
            skills_dir: PathBuf::from(".agents/skills"),
            pause_tx: None,
            dangerous_commands: DangerousCommandsConfig::default(),
            frontend_capabilities: FrontendCapability::text_only(),
            frontend_extensions: Vec::new(),
            frontend_events: FrontendEventSink::default(),
            extension_state: None,
            extension_id: None,
        }
    }

    pub fn for_extension(&self, extension_id: impl Into<String>) -> Self {
        let mut ctx = self.clone();
        ctx.extension_id = Some(extension_id.into());
        ctx
    }

    pub fn with_frontend_extensions(
        mut self,
        frontend_extensions: Vec<vulcan_frontend_api::FrontendExtensionDescriptor>,
    ) -> Self {
        self.frontend_extensions = frontend_extensions;
        self
    }

    pub fn with_frontend_capabilities(mut self, capabilities: Vec<FrontendCapability>) -> Self {
        self.frontend_capabilities = capabilities;
        self
    }

    pub fn with_skills_dir(mut self, skills_dir: PathBuf) -> Self {
        self.skills_dir = skills_dir;
        self
    }

    pub fn with_pause_channel(mut self, pause_tx: Option<PauseSender>) -> Self {
        self.pause_tx = pause_tx;
        self
    }

    pub fn with_dangerous_commands(mut self, dangerous_commands: DangerousCommandsConfig) -> Self {
        self.dangerous_commands = dangerous_commands;
        self
    }

    pub fn with_extension_state_store(
        mut self,
        extension_state: Option<Arc<dyn ExtensionStateStore>>,
    ) -> Self {
        self.extension_state = extension_state;
        self
    }

    pub fn state(&self) -> Result<ExtensionStateScope> {
        let Some(extension_id) = &self.extension_id else {
            anyhow::bail!("extension state requires extension-scoped context");
        };
        let Some(store) = &self.extension_state else {
            anyhow::bail!("extension state store is unavailable");
        };
        store.scope(extension_id)
    }

    pub fn emit_frontend_event(&self, payload: Value) -> Result<()> {
        let Some(extension_id) = &self.extension_id else {
            anyhow::bail!("frontend event emission requires extension-scoped context");
        };
        self.frontend_events.emit(FrontendEvent {
            session_id: self.session_id.clone(),
            extension_id: extension_id.clone(),
            payload,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrontendEvent {
    pub session_id: String,
    pub extension_id: String,
    pub payload: Value,
}

#[derive(Clone, Debug, Default)]
pub struct FrontendEventSink {
    tx: Option<broadcast::Sender<FrontendEvent>>,
}

impl FrontendEventSink {
    pub fn new(tx: broadcast::Sender<FrontendEvent>) -> Self {
        Self { tx: Some(tx) }
    }

    pub fn emit(&self, event: FrontendEvent) -> Result<()> {
        if let Some(tx) = &self.tx {
            let _ = tx.send(event);
        }
        Ok(())
    }
}

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

    /// Configuration fields this daemon extension declares.
    /// The `vulcan config` CLI surfaces active extension fields
    /// under the extension's id.
    fn config_fields(&self) -> Vec<ExtensionConfigField> {
        Vec::new()
    }
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

    /// Typed orchestration hooks this session extension contributes.
    /// GH issue #271 only emits delegation events today because typed
    /// plan/step runtime objects do not exist yet.
    fn orchestration_hooks(&self) -> Vec<Arc<dyn OrchestrationHook>> {
        Vec::new()
    }

    /// Commands this session extension contributes.
    fn commands(&self) -> Vec<Arc<dyn ExtensionCommand>> {
        Vec::new()
    }

    /// Concrete provider instances this session extension contributes.
    fn providers(&self) -> Vec<Box<dyn LLMProvider>> {
        Vec::new()
    }

    /// Provider factories this session extension contributes.
    fn provider_factories(&self) -> Vec<Arc<dyn ProviderFactory>> {
        Vec::new()
    }

    /// Lifecycle hook invoked when the session extension is activated.
    async fn on_activate(&self) {}

    /// Lifecycle hook invoked when the session extension is deactivated.
    async fn on_deactivate(&self) {}
}

#[derive(Clone)]
pub struct SessionExtensionRuntime {
    id: String,
    extension: Arc<dyn SessionExtension>,
    draining: Arc<AtomicBool>,
    session_id: String,
    frontend_events: FrontendEventSink,
}

impl SessionExtensionRuntime {
    pub fn new(id: impl Into<String>, extension: Arc<dyn SessionExtension>) -> Self {
        Self {
            id: id.into(),
            extension,
            draining: Arc::new(AtomicBool::new(false)),
            session_id: String::new(),
            frontend_events: FrontendEventSink::default(),
        }
    }

    pub fn new_with_context(
        id: impl Into<String>,
        extension: Arc<dyn SessionExtension>,
        ctx: &SessionExtensionCtx,
    ) -> Self {
        Self {
            id: id.into(),
            extension,
            draining: Arc::new(AtomicBool::new(false)),
            session_id: ctx.session_id.clone(),
            frontend_events: ctx.frontend_events.clone(),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    pub fn set_draining(&self, draining: bool) {
        self.draining.store(draining, Ordering::SeqCst);
    }

    pub async fn deactivate(&self) {
        self.extension.on_deactivate().await;
    }

    pub async fn activate(&self) {
        self.extension.on_activate().await;
    }

    pub fn emit_lifecycle_success(&self, reason: &str, message: impl Into<String>) {
        self.emit_lifecycle("success", reason, message);
    }

    pub fn emit_lifecycle_warning(&self, reason: &str, message: impl Into<String>) {
        self.emit_lifecycle("warning", reason, message);
    }

    pub fn emit_lifecycle_failure(&self, reason: &str, message: impl Into<String>) {
        self.emit_lifecycle("failure", reason, message);
    }

    pub fn emit_lifecycle_skipped(&self, reason: &str, message: impl Into<String>) {
        self.emit_lifecycle("skipped", reason, message);
    }

    fn emit_lifecycle(&self, status: &str, reason: &str, message: impl Into<String>) {
        let _ = self.frontend_events.emit(FrontendEvent {
            session_id: self.session_id.clone(),
            extension_id: self.id.clone(),
            payload: serde_json::json!({
                "kind": "lifecycle",
                "status": status,
                "reason": reason,
                "message": message.into(),
            }),
        });
    }

    pub fn wrapped_hook_handlers(&self) -> Vec<Arc<dyn HookHandler>> {
        self.extension.hook_handlers()
    }

    pub fn wrapped_tools(&self) -> Vec<Arc<dyn Tool>> {
        self.extension
            .tools()
            .into_iter()
            .map(|inner| {
                Arc::new(DrainingTool {
                    inner,
                    draining: Arc::clone(&self.draining),
                }) as Arc<dyn Tool>
            })
            .collect()
    }

    pub fn orchestration_hooks(&self) -> Vec<Arc<dyn OrchestrationHook>> {
        self.extension.orchestration_hooks()
    }
}

struct DrainingTool {
    inner: Arc<dyn Tool>,
    draining: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl Tool for DrainingTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn schema(&self) -> Value {
        self.inner.schema()
    }

    async fn call(
        &self,
        params: Value,
        cancel: CancellationToken,
        progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        if self.draining.load(Ordering::SeqCst) {
            return Ok(ToolResult::err("extension draining"));
        }
        self.inner.call(params, cancel, progress).await
    }

    fn is_relevant(&self, ctx: &crate::tools::ToolContext) -> bool {
        self.inner.is_relevant(ctx)
    }

    fn dynamic_description(&self, ctx: &crate::tools::ToolContext) -> Option<String> {
        self.inner.dynamic_description(ctx)
    }
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
pub(crate) async fn test_session_extension_ctx() -> SessionExtensionCtx {
    SessionExtensionCtx::new(
        PathBuf::from("/tmp/test-session"),
        "test-session-id".to_string(),
        Arc::new(SessionStore::in_memory().await),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::ExtensionSource;

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
    async fn test_ctx() -> SessionExtensionCtx {
        test_session_extension_ctx().await
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

    #[tokio::test]
    async fn wire_daemon_extensions_instantiates_and_registers_hook_handlers_per_session() {
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

        let hooks = HookRegistry::new();
        let registered = registry.wire_daemon_extensions(test_ctx().await, &hooks);

        assert_eq!(registered, 1);
        assert_eq!(hooks.handler_count(), 1);
    }

    #[tokio::test]
    async fn session_extension_ctx_carries_cwd_and_session_id_to_instantiate() {
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
        let ctx = SessionExtensionCtx::new(
            PathBuf::from("/tmp/example-session"),
            "sess-42".to_string(),
            Arc::new(SessionStore::in_memory().await),
        );
        let _ = ext.instantiate(ctx);

        let captured = seen.read().clone().expect("instantiate ran");
        assert_eq!(captured.0, PathBuf::from("/tmp/example-session"));
        assert_eq!(captured.1, "sess-42");
    }

    #[tokio::test]
    async fn session_extension_runtime_emits_lifecycle_events() {
        struct NoopSession;
        impl SessionExtension for NoopSession {}

        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let ctx = SessionExtensionCtx {
            frontend_events: FrontendEventSink::new(tx),
            ..test_ctx().await
        };
        let runtime =
            SessionExtensionRuntime::new_with_context("life-ext", Arc::new(NoopSession), &ctx);

        runtime.emit_lifecycle_success("activated", "extension activated");
        runtime.emit_lifecycle_failure("tool_registration_failed", "duplicate tool");

        let success = rx.recv().await.unwrap();
        assert_eq!(success.session_id, ctx.session_id);
        assert_eq!(success.extension_id, "life-ext");
        assert_eq!(success.payload["kind"], "lifecycle");
        assert_eq!(success.payload["status"], "success");
        assert_eq!(success.payload["reason"], "activated");

        let failure = rx.recv().await.unwrap();
        assert_eq!(failure.extension_id, "life-ext");
        assert_eq!(failure.payload["kind"], "lifecycle");
        assert_eq!(failure.payload["status"], "failure");
        assert_eq!(failure.payload["reason"], "tool_registration_failed");
        assert_eq!(failure.payload["message"], "duplicate tool");
    }

    #[tokio::test]
    async fn extension_state_requires_scoped_context_and_uses_extension_id() {
        let store: Arc<dyn ExtensionStateStore> =
            Arc::new(super::super::SqliteExtensionStateStore::try_open_in_memory().unwrap());
        let ctx = test_ctx().await.with_extension_state_store(Some(store));
        assert!(ctx.state().is_err());

        let scoped = ctx.for_extension("stateful-ext").state().unwrap();
        scoped
            .put_json("draft", &serde_json::json!({"step": 2}))
            .unwrap();
        assert_eq!(
            scoped.get_json("draft").unwrap(),
            Some(serde_json::json!({"step": 2}))
        );
        assert_eq!(scoped.extension_id(), "stateful-ext");
    }

    #[tokio::test]
    async fn instantiated_session_extension_exposes_its_hook_handlers() {
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
        let session_ext = daemon_ext.instantiate(test_ctx().await);
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

    #[tokio::test]
    async fn draining_runtime_skips_prompt_hooks_but_keeps_handler_registered() {
        use crate::hooks::{HookOutcome, HookRegistry, InjectPosition};
        use crate::provider::Message;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct PromptSession {
            calls: Arc<AtomicUsize>,
        }

        impl SessionExtension for PromptSession {
            fn hook_handlers(&self) -> Vec<Arc<dyn HookHandler>> {
                vec![Arc::new(PromptHook {
                    calls: Arc::clone(&self.calls),
                })]
            }
        }

        struct PromptHook {
            calls: Arc<AtomicUsize>,
        }

        #[async_trait::async_trait]
        impl HookHandler for PromptHook {
            fn name(&self) -> &str {
                "prompt-hook"
            }

            async fn before_prompt(
                &self,
                _messages: &[Message],
                _cancel: CancellationToken,
            ) -> Result<HookOutcome> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(HookOutcome::InjectMessages {
                    messages: vec![Message::System {
                        content: "injected".into(),
                    }],
                    position: InjectPosition::Append,
                })
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let runtime = SessionExtensionRuntime::new(
            "prompt-ext",
            Arc::new(PromptSession {
                calls: Arc::clone(&calls),
            }),
        );
        let hooks = HookRegistry::new();
        for handler in runtime.wrapped_hook_handlers() {
            hooks.register_extension_handler(runtime.id(), handler);
        }
        assert_eq!(hooks.handler_count(), 1);

        let input = vec![Message::User {
            content: "hi".into(),
        }];
        let out = hooks
            .apply_before_prompt(&input, CancellationToken::new())
            .await;
        assert_eq!(out.len(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        assert!(hooks.set_extension_draining(runtime.id(), true));
        let out = hooks
            .apply_before_prompt(&input, CancellationToken::new())
            .await;
        assert_eq!(out.len(), 1);
        assert!(matches!(
            out.first(),
            Some(Message::User { content }) if content == "hi"
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn draining_runtime_refuses_new_tool_calls() {
        struct ToolSession;
        impl SessionExtension for ToolSession {
            fn tools(&self) -> Vec<Arc<dyn Tool>> {
                vec![Arc::new(EchoTool)]
            }
        }

        struct EchoTool;
        #[async_trait::async_trait]
        impl Tool for EchoTool {
            fn name(&self) -> &str {
                "echo_ext"
            }

            fn description(&self) -> &str {
                "echo"
            }

            fn schema(&self) -> Value {
                serde_json::json!({})
            }

            async fn call(
                &self,
                _params: Value,
                _cancel: CancellationToken,
                _progress: Option<crate::tools::ProgressSink>,
            ) -> Result<ToolResult> {
                Ok(ToolResult::ok("ran"))
            }
        }

        let runtime = SessionExtensionRuntime::new("tool-ext", Arc::new(ToolSession));
        let tool = runtime.wrapped_tools().remove(0);
        let result = tool
            .call(serde_json::json!({}), CancellationToken::new(), None)
            .await
            .unwrap();
        assert_eq!(result.output, "ran");
        assert!(!result.is_error);

        runtime.set_draining(true);
        let result = tool
            .call(serde_json::json!({}), CancellationToken::new(), None)
            .await
            .unwrap();
        assert_eq!(result.output, "extension draining");
        assert!(result.is_error);
    }
}
