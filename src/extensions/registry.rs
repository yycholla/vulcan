//! YYC-165 PR-1: extension registry.
//!
//! Today the registry is metadata-only — call sites can register
//! `ExtensionMetadata`, query by id / capability, and snapshot
//! the deterministic load order. Subsequent PRs in YYC-165 layer
//! draft parsing, config gating, and code-backed activation on
//! top of this surface.

use parking_lot::RwLock;

use super::{ExtensionCapability, ExtensionMetadata, ExtensionStatus};

#[derive(Debug, Default)]
pub struct ExtensionRegistry {
    inner: RwLock<Vec<ExtensionMetadata>>,
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

    #[test]
    fn metadata_round_trips_through_serde_json() {
        let m = meta("alpha", 10);
        let json = serde_json::to_string(&m).unwrap();
        let back: ExtensionMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
    }
}
