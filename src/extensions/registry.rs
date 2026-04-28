//! YYC-165 PR-1: extension registry.
//!
//! Today the registry is metadata-only — call sites can register
//! `ExtensionMetadata`, query by id / capability, and snapshot
//! the deterministic load order. Subsequent PRs in YYC-165 layer
//! draft parsing, config gating, and code-backed activation on
//! top of this surface.

use parking_lot::RwLock;
use std::sync::Arc;

use super::{ExtensionCapability, ExtensionConfigField, ExtensionMetadata, ExtensionStatus};
use crate::hooks::{HookHandler, HookRegistry};

/// YYC-227 (YYC-165 PR-4): trait an in-process, code-backed
/// extension implements. Implementors live alongside the
/// [`ExtensionMetadata`] in the registry; only `Active`
/// extensions get their hook handlers wired into the live
/// `HookRegistry` at session start.
pub trait CodeExtension: Send + Sync {
    /// The static metadata describing this extension. Must
    /// match the metadata under which the registry indexed the
    /// extension.
    fn metadata(&self) -> ExtensionMetadata;
    /// Hook handlers this extension contributes. Called once,
    /// at wire-time. Default implementation returns nothing —
    /// extensions that only contribute prompt injections via a
    /// `BeforePrompt` handler can override just this method.
    fn hook_handlers(&self) -> Vec<Arc<dyn HookHandler>> {
        Vec::new()
    }

    /// YYC-228: configuration fields this extension declares.
    /// The YYC-212 `vulcan config` CLI surfaces them under the
    /// extension's id. Default returns nothing.
    fn config_fields(&self) -> Vec<ExtensionConfigField> {
        Vec::new()
    }
}

#[derive(Default)]
pub struct ExtensionRegistry {
    inner: RwLock<Vec<ExtensionMetadata>>,
    /// YYC-227: code-backed extensions, indexed by id. The
    /// registry does not own metadata for these separately —
    /// `metadata()` on the trait is the source of truth.
    code_backed: RwLock<Vec<Arc<dyn CodeExtension>>>,
}

impl std::fmt::Debug for ExtensionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionRegistry")
            .field("metadata", &self.inner.read().clone())
            .field("code_backed_count", &self.code_backed.read().len())
            .finish()
    }
}

impl ExtensionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace an extension by id. Returns `true` when
    /// a previous entry was overwritten.
    pub fn upsert(&self, metadata: ExtensionMetadata) -> bool {
        let mut guard = self.inner.write();
        let mut replaced = false;
        if let Some(slot) = guard.iter_mut().find(|m| m.id == metadata.id) {
            *slot = metadata;
            replaced = true;
        } else {
            guard.push(metadata);
        }
        Self::sort_in_place(&mut guard);
        replaced
    }

    /// Remove an extension by id. Returns `true` when an entry
    /// existed.
    pub fn remove(&self, id: &str) -> bool {
        let mut guard = self.inner.write();
        let before = guard.len();
        guard.retain(|m| m.id != id);
        guard.len() != before
    }

    /// Snapshot of registered metadata in deterministic load
    /// order: priority asc, then id asc.
    pub fn list(&self) -> Vec<ExtensionMetadata> {
        self.inner.read().clone()
    }

    pub fn get(&self, id: &str) -> Option<ExtensionMetadata> {
        self.inner.read().iter().find(|m| m.id == id).cloned()
    }

    /// All `Active` extensions whose declared capabilities
    /// contain `cap`. Used by future PRs to enumerate which
    /// extensions claim e.g. tool-provider rights when wiring
    /// the registry.
    pub fn active_with_capability(&self, cap: ExtensionCapability) -> Vec<ExtensionMetadata> {
        self.inner
            .read()
            .iter()
            .filter(|m| m.status == ExtensionStatus::Active && m.capabilities.contains(&cap))
            .cloned()
            .collect()
    }

    /// Update an extension's status without touching the rest of
    /// its metadata. Returns `true` when the extension exists.
    pub fn set_status(&self, id: &str, status: ExtensionStatus) -> bool {
        let mut guard = self.inner.write();
        if let Some(slot) = guard.iter_mut().find(|m| m.id == id) {
            slot.status = status;
            slot.broken_reason = None;
            true
        } else {
            false
        }
    }

    /// Mark an extension `Broken` with a diagnostic reason.
    pub fn mark_broken(&self, id: &str, reason: impl Into<String>) -> bool {
        let mut guard = self.inner.write();
        if let Some(slot) = guard.iter_mut().find(|m| m.id == id) {
            slot.status = ExtensionStatus::Broken;
            slot.broken_reason = Some(reason.into());
            true
        } else {
            false
        }
    }

    fn sort_in_place(items: &mut [ExtensionMetadata]) {
        items.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));
    }

    /// YYC-227: register a code-backed extension. The registry
    /// upserts its metadata under the extension's declared id +
    /// stores the trait object so it can be activated later.
    pub fn register_code_extension(&self, extension: Arc<dyn CodeExtension>) {
        let metadata = extension.metadata();
        self.upsert(metadata.clone());
        let mut guard = self.code_backed.write();
        if let Some(slot) = guard.iter_mut().find(|e| e.metadata().id == metadata.id) {
            *slot = extension;
        } else {
            guard.push(extension);
        }
    }

    /// YYC-227: register every `Active` code-backed extension's
    /// hook handlers into the live [`HookRegistry`]. Returns the
    /// number of handlers registered.
    pub fn wire_into_hooks(&self, hooks: &mut HookRegistry) -> usize {
        let mut total = 0usize;
        let metadata_snapshot = self.inner.read().clone();
        let code_snapshot = self.code_backed.read().clone();
        for ext in code_snapshot {
            let id = ext.metadata().id;
            let active = metadata_snapshot
                .iter()
                .find(|m| m.id == id)
                .map(|m| m.status == ExtensionStatus::Active)
                .unwrap_or(false);
            if !active {
                continue;
            }
            for handler in ext.hook_handlers() {
                hooks.register(handler);
                total += 1;
            }
        }
        total
    }

    /// YYC-227: count of code-backed extensions registered.
    pub fn code_extension_count(&self) -> usize {
        self.code_backed.read().len()
    }

    /// YYC-228: enumerate every config field contributed by an
    /// `Active` extension. Returns `(extension_id, field)` pairs
    /// so callers can prefix the id when displaying.
    /// Inactive / Draft / Broken extensions contribute nothing.
    pub fn active_config_fields(&self) -> Vec<(String, ExtensionConfigField)> {
        let metadata_snapshot = self.inner.read().clone();
        let code_snapshot = self.code_backed.read().clone();
        let mut out = Vec::new();
        for ext in code_snapshot {
            let id = ext.metadata().id;
            let active = metadata_snapshot
                .iter()
                .find(|m| m.id == id)
                .map(|m| m.status == ExtensionStatus::Active)
                .unwrap_or(false);
            if !active {
                continue;
            }
            for field in ext.config_fields() {
                out.push((id.clone(), field));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::ExtensionSource;

    fn meta(id: &str, priority: i32) -> ExtensionMetadata {
        let mut m = ExtensionMetadata::new(id, id, "0.1.0", ExtensionSource::Builtin);
        m.priority = priority;
        m
    }

    #[test]
    fn upsert_inserts_new_and_overwrites_existing() {
        let reg = ExtensionRegistry::new();
        assert!(!reg.upsert(meta("alpha", 10)));
        assert_eq!(reg.list().len(), 1);
        let mut updated = meta("alpha", 10);
        updated.description = "updated body".into();
        assert!(reg.upsert(updated));
        assert_eq!(reg.get("alpha").unwrap().description, "updated body");
    }

    #[test]
    fn list_returns_priority_asc_then_id_asc() {
        let reg = ExtensionRegistry::new();
        reg.upsert(meta("delta", 50));
        reg.upsert(meta("alpha", 10));
        reg.upsert(meta("charlie", 10));
        reg.upsert(meta("bravo", 10));
        let ids: Vec<String> = reg.list().into_iter().map(|m| m.id).collect();
        // Tied priorities sort by id; lower priority first.
        assert_eq!(ids, vec!["alpha", "bravo", "charlie", "delta"]);
    }

    #[test]
    fn remove_returns_false_when_id_missing() {
        let reg = ExtensionRegistry::new();
        reg.upsert(meta("alpha", 10));
        assert!(!reg.remove("ghost"));
        assert!(reg.remove("alpha"));
        assert!(reg.list().is_empty());
    }

    #[test]
    fn active_with_capability_filters_by_status_and_cap() {
        let reg = ExtensionRegistry::new();
        let mut active_hook = meta("with-hook", 10);
        active_hook.status = ExtensionStatus::Active;
        active_hook.capabilities = vec![ExtensionCapability::HookHandler];
        let mut inactive_hook = meta("inactive-hook", 20);
        inactive_hook.capabilities = vec![ExtensionCapability::HookHandler];
        let mut active_tool = meta("with-tool", 30);
        active_tool.status = ExtensionStatus::Active;
        active_tool.capabilities = vec![ExtensionCapability::ToolProvider];
        reg.upsert(active_hook);
        reg.upsert(inactive_hook);
        reg.upsert(active_tool);

        let hooks = reg.active_with_capability(ExtensionCapability::HookHandler);
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].id, "with-hook");

        let tools = reg.active_with_capability(ExtensionCapability::ToolProvider);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].id, "with-tool");
    }

    #[test]
    fn mark_broken_sets_status_and_reason() {
        let reg = ExtensionRegistry::new();
        reg.upsert(meta("alpha", 10));
        assert!(reg.mark_broken("alpha", "manifest invalid"));
        let got = reg.get("alpha").unwrap();
        assert_eq!(got.status, ExtensionStatus::Broken);
        assert_eq!(got.broken_reason.as_deref(), Some("manifest invalid"));
    }

    #[test]
    fn set_status_clears_broken_reason() {
        let reg = ExtensionRegistry::new();
        reg.upsert(meta("alpha", 10));
        reg.mark_broken("alpha", "transient");
        assert!(reg.set_status("alpha", ExtensionStatus::Active));
        let got = reg.get("alpha").unwrap();
        assert_eq!(got.status, ExtensionStatus::Active);
        assert_eq!(got.broken_reason, None);
    }

    // ── YYC-227 (YYC-165 PR-4): code-backed wiring ──────────────────

    use anyhow::Result;
    use async_trait::async_trait;
    use tokio_util::sync::CancellationToken;

    struct StubExtension {
        meta: ExtensionMetadata,
    }

    impl StubExtension {
        fn new(id: &str, status: ExtensionStatus) -> Self {
            let mut m = ExtensionMetadata::new(
                id,
                id,
                "0.1.0",
                crate::extensions::ExtensionSource::Builtin,
            );
            m.status = status;
            m.capabilities = vec![ExtensionCapability::HookHandler];
            Self { meta: m }
        }
    }

    impl crate::extensions::CodeExtension for StubExtension {
        fn metadata(&self) -> ExtensionMetadata {
            self.meta.clone()
        }
        fn hook_handlers(&self) -> Vec<Arc<dyn crate::hooks::HookHandler>> {
            vec![Arc::new(NoopHook {
                id: self.meta.id.clone(),
            })]
        }
    }

    struct NoopHook {
        id: String,
    }

    #[async_trait]
    impl crate::hooks::HookHandler for NoopHook {
        fn name(&self) -> &str {
            &self.id
        }
        async fn before_prompt(
            &self,
            _messages: &[crate::provider::Message],
            _cancel: CancellationToken,
        ) -> Result<crate::hooks::HookOutcome> {
            Ok(crate::hooks::HookOutcome::Continue)
        }
    }

    #[test]
    fn wire_into_hooks_skips_inactive_extensions() {
        let reg = ExtensionRegistry::new();
        reg.register_code_extension(Arc::new(StubExtension::new(
            "active-one",
            ExtensionStatus::Active,
        )));
        reg.register_code_extension(Arc::new(StubExtension::new(
            "inactive-one",
            ExtensionStatus::Inactive,
        )));
        reg.register_code_extension(Arc::new(StubExtension::new(
            "draft-one",
            ExtensionStatus::Draft,
        )));
        reg.register_code_extension(Arc::new(StubExtension::new(
            "broken-one",
            ExtensionStatus::Broken,
        )));

        let mut hooks = crate::hooks::HookRegistry::new();
        let registered = reg.wire_into_hooks(&mut hooks);
        assert_eq!(registered, 1);
        assert_eq!(hooks.handler_count(), 1);
    }

    #[test]
    fn wire_into_hooks_respects_status_changes() {
        let reg = ExtensionRegistry::new();
        // Start inactive — wiring registers nothing.
        reg.register_code_extension(Arc::new(StubExtension::new(
            "toggle",
            ExtensionStatus::Inactive,
        )));
        let mut hooks = crate::hooks::HookRegistry::new();
        assert_eq!(reg.wire_into_hooks(&mut hooks), 0);
        // Promote to active and re-wire.
        assert!(reg.set_status("toggle", ExtensionStatus::Active));
        let mut hooks = crate::hooks::HookRegistry::new();
        assert_eq!(reg.wire_into_hooks(&mut hooks), 1);
    }

    #[test]
    fn register_code_extension_replaces_metadata_on_id_collision() {
        let reg = ExtensionRegistry::new();
        reg.register_code_extension(Arc::new(StubExtension::new(
            "dup",
            ExtensionStatus::Inactive,
        )));
        reg.register_code_extension(Arc::new(StubExtension::new("dup", ExtensionStatus::Active)));
        assert_eq!(reg.code_extension_count(), 1);
        assert_eq!(reg.get("dup").unwrap().status, ExtensionStatus::Active);
    }

    #[test]
    fn active_config_fields_only_includes_active_contributors() {
        struct ExtWithFields {
            meta: ExtensionMetadata,
        }
        impl crate::extensions::CodeExtension for ExtWithFields {
            fn metadata(&self) -> ExtensionMetadata {
                self.meta.clone()
            }
            fn config_fields(&self) -> Vec<ExtensionConfigField> {
                vec![ExtensionConfigField::bool_field("enabled", false, "toggle")]
            }
        }
        let reg = ExtensionRegistry::new();
        let mut active = ExtensionMetadata::new(
            "active-fielded",
            "active",
            "0.1.0",
            crate::extensions::ExtensionSource::Builtin,
        );
        active.status = ExtensionStatus::Active;
        let mut inactive = ExtensionMetadata::new(
            "inactive-fielded",
            "inactive",
            "0.1.0",
            crate::extensions::ExtensionSource::Builtin,
        );
        inactive.status = ExtensionStatus::Inactive;
        reg.register_code_extension(Arc::new(ExtWithFields { meta: active }));
        reg.register_code_extension(Arc::new(ExtWithFields { meta: inactive }));

        let fields = reg.active_config_fields();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].0, "active-fielded");
        assert_eq!(fields[0].1.path, "enabled");
    }

    #[test]
    fn metadata_round_trips_through_serde_json() {
        let m = meta("alpha", 10);
        let json = serde_json::to_string(&m).unwrap();
        let back: ExtensionMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
    }
}
