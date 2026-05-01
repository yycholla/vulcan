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

use super::ExtensionMetadata;
use crate::hooks::HookHandler;

/// Per-**Session** instantiation context handed to a
/// `DaemonCodeExtension::instantiate` call. Carries the bare minimum
/// the auto-commit dogfood needs today; grows toward the full
/// `ExtensionContext` (model, provider, pause, state, ui, ...) as
/// later slices land.
#[derive(Debug, Clone)]
pub struct SessionExtensionCtx {
    /// Working directory of the **Session** the extension is being
    /// instantiated for. Auto-commit reads this to know which repo to
    /// `git commit` against.
    pub cwd: PathBuf,
    /// **Session** identifier. Routes telemetry, audit log entries,
    /// and per-session state under a stable key.
    pub session_id: String,
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
}

/// Per-**Session** instantiation of a `DaemonCodeExtension`. Owns
/// hooks, tools, commands, providers, and lifecycle handlers.
///
/// All methods default-empty so an extension can opt into only the
/// surfaces it needs.
pub trait SessionExtension: Send + Sync {
    /// Hook handlers this **Session Extension** contributes. Wired
    /// into the session's `HookRegistry` once at session construction.
    /// Default returns nothing — extensions that don't observe hook
    /// events leave this unimplemented.
    fn hook_handlers(&self) -> Vec<Arc<dyn HookHandler>> {
        Vec::new()
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
    fn test_ctx() -> SessionExtensionCtx {
        SessionExtensionCtx {
            cwd: PathBuf::from("/tmp/test-session"),
            session_id: "test-session-id".to_string(),
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
        };
        let _ = ext.instantiate(ctx);

        let captured = seen.read().clone().expect("instantiate ran");
        assert_eq!(captured.0, PathBuf::from("/tmp/example-session"));
        assert_eq!(captured.1, "sess-42");
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
}
