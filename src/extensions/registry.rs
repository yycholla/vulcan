//! YYC-165 PR-1: extension registry.
//!
//! Today the registry is metadata-only — call sites can register
//! `ExtensionMetadata`, query by id / capability, and snapshot
//! the deterministic load order. Subsequent PRs in YYC-165 layer
//! draft parsing, config gating, and code-backed activation on
//! top of this surface.

use parking_lot::RwLock;
use std::sync::Arc;

use super::api::DaemonCodeExtension;
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
    /// GH issue #549: cargo-crate extensions registered via the
    /// `inventory::submit!` site in `super::api`. Parallel storage
    /// to `code_backed` while migration is in flight.
    daemon_extensions: RwLock<Vec<Arc<dyn DaemonCodeExtension>>>,
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

    /// GH issue #549: register a cargo-crate `DaemonCodeExtension`.
    /// Upserts metadata and stores the trait object so daemon startup
    /// can later instantiate it per-**Session**.
    pub fn register_daemon_extension(&self, extension: Arc<dyn DaemonCodeExtension>) {
        let metadata = extension.metadata();
        self.upsert(metadata.clone());
        let mut guard = self.daemon_extensions.write();
        if let Some(slot) = guard.iter_mut().find(|e| e.metadata().id == metadata.id) {
            *slot = extension;
        } else {
            guard.push(extension);
        }
    }

    /// GH issue #549: count of cargo-crate extensions registered via
    /// the new `DaemonCodeExtension` path.
    pub fn daemon_extension_count(&self) -> usize {
        self.daemon_extensions.read().len()
    }

    /// GH issue #549: per-**Session** wire-up. For each `Active`
    /// daemon-side extension registered via `register_daemon_extension`,
    /// calls `instantiate` to get a fresh `SessionExtension`, then
    /// registers each of that session extension's `hook_handlers` into
    /// the supplied `HookRegistry`. Returns the number of session
    /// extensions instantiated. Inactive / Draft / Broken extensions
    /// are skipped.
    pub fn wire_daemon_extensions(&self, hooks: &mut HookRegistry) -> usize {
        let mut count = 0usize;
        let metadata_snapshot = self.inner.read().clone();
        let daemon_snapshot = self.daemon_extensions.read().clone();
        for ext in daemon_snapshot {
            let id = ext.metadata().id;
            let active = metadata_snapshot
                .iter()
                .find(|m| m.id == id)
                .map(|m| m.status == ExtensionStatus::Active)
                .unwrap_or(false);
            if !active {
                continue;
            }
            let session_ext = ext.instantiate();
            for handler in session_ext.hook_handlers() {
                hooks.register(handler);
            }
            count += 1;
        }
        count
    }

    /// YYC-232 (YYC-166 PR-4): discover installed extensions in
    /// `home`, upsert their metadata as `LocalManifest`, and
    /// honor each id's `InstallState.enabled` flag when picking
    /// status. Manifest parse failures land as `Broken` with
    /// the parse-error message recorded on the install state
    /// row.
    ///
    /// Returns `(loaded_ok, broken)` so callers can log
    /// per-startup health.
    pub fn load_from_store(
        &self,
        home: &std::path::Path,
        install_state: &dyn super::install_state::InstallStateStore,
    ) -> (usize, usize) {
        self.load_from_store_with_version(home, install_state, env!("CARGO_PKG_VERSION"))
    }

    /// Test-friendly variant — `running_version` lets fixtures
    /// pin a value instead of inheriting `CARGO_PKG_VERSION`.
    pub fn load_from_store_with_version(
        &self,
        home: &std::path::Path,
        install_state: &dyn super::install_state::InstallStateStore,
        running_version: &str,
    ) -> (usize, usize) {
        let discovered = super::store::discover(home);
        let mut ok = 0usize;
        let mut broken = 0usize;
        for entry in discovered {
            let dir_id = entry.dir_id.clone();
            match (entry.manifest, entry.parse_error) {
                (Some(manifest), _) => {
                    // YYC-233: refuse manifests demanding a
                    // newer Vulcan than the runtime. Surface as
                    // Broken with the verifier's reason; record
                    // the same on the install_state row.
                    if let Err(err) = super::verify::verify_compatible(&manifest, running_version) {
                        let reason = err.to_string();
                        let mut meta = ExtensionMetadata::new(
                            manifest.id.clone(),
                            manifest.name.clone(),
                            manifest.version.clone(),
                            crate::extensions::ExtensionSource::LocalManifest,
                        );
                        meta.status = ExtensionStatus::Broken;
                        meta.broken_reason = Some(reason.clone());
                        self.upsert(meta);
                        let _ = install_state.record_load_error(&manifest.id, &reason);
                        broken += 1;
                        continue;
                    }

                    let mut meta = ExtensionMetadata::new(
                        manifest.id.clone(),
                        manifest.name.clone(),
                        manifest.version.clone(),
                        crate::extensions::ExtensionSource::LocalManifest,
                    );
                    if let Some(desc) = manifest.description.clone() {
                        meta.description = desc;
                    }
                    if let Some(perm) = manifest.permissions.clone() {
                        meta.permissions_summary = Some(perm);
                    }
                    // Honor install state when present; default
                    // to Inactive otherwise so freshly-dropped
                    // extensions stay quiet until the user opts
                    // in.
                    let enabled = install_state
                        .get(&manifest.id)
                        .ok()
                        .flatten()
                        .map(|s| s.enabled)
                        .unwrap_or(false);
                    meta.status = if enabled {
                        ExtensionStatus::Active
                    } else {
                        ExtensionStatus::Inactive
                    };
                    self.upsert(meta);
                    let _ = install_state.clear_load_error(&manifest.id);
                    ok += 1;
                }
                (None, Some(err)) => {
                    let reason = err.to_string();
                    let mut meta = ExtensionMetadata::new(
                        dir_id.clone(),
                        dir_id.clone(),
                        "0.0.0",
                        crate::extensions::ExtensionSource::LocalManifest,
                    );
                    meta.status = ExtensionStatus::Broken;
                    meta.broken_reason = Some(reason.clone());
                    self.upsert(meta);
                    let _ = install_state.record_load_error(&dir_id, &reason);
                    broken += 1;
                }
                _ => {}
            }
        }
        (ok, broken)
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

    // ── YYC-232 (YYC-166 PR-4): store + install_state bridge ────────

    fn write_manifest(path: std::path::PathBuf, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn load_from_store_imports_manifests_with_inactive_default() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path().join("extensions/lint-helper/extension.toml"),
            r#"
id = "lint-helper"
name = "Lint Helper"
version = "0.1.0"

[entry]
kind = "builtin"
"#,
        );
        let install =
            crate::extensions::install_state::SqliteInstallStateStore::try_open_in_memory()
                .unwrap();
        let reg = ExtensionRegistry::new();
        let (ok, broken) = reg.load_from_store(dir.path(), &install);
        assert_eq!(ok, 1);
        assert_eq!(broken, 0);
        let got = reg.get("lint-helper").unwrap();
        assert_eq!(got.status, ExtensionStatus::Inactive);
        assert_eq!(
            got.source,
            crate::extensions::ExtensionSource::LocalManifest
        );
    }

    #[test]
    fn load_from_store_promotes_to_active_when_install_state_says_enabled() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path().join("extensions/active-tool/extension.toml"),
            r#"
id = "active-tool"
name = "Active Tool"
version = "0.1.0"

[entry]
kind = "builtin"
"#,
        );
        let install =
            crate::extensions::install_state::SqliteInstallStateStore::try_open_in_memory()
                .unwrap();
        crate::extensions::install_state::InstallStateStore::upsert(
            &install,
            &crate::extensions::install_state::InstallState {
                id: "active-tool".into(),
                version: "0.1.0".into(),
                enabled: true,
                installed_at: chrono::Utc::now(),
                last_load_error: None,
            },
        )
        .unwrap();
        let reg = ExtensionRegistry::new();
        reg.load_from_store(dir.path(), &install);
        assert_eq!(
            reg.get("active-tool").unwrap().status,
            ExtensionStatus::Active
        );
    }

    #[test]
    fn load_from_store_rejects_manifest_demanding_newer_vulcan() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path().join("extensions/needs-future/extension.toml"),
            r#"
id = "needs-future"
name = "Needs Future"
version = "0.1.0"
min_vulcan_version = "9.0.0"

[entry]
kind = "builtin"
"#,
        );
        let install =
            crate::extensions::install_state::SqliteInstallStateStore::try_open_in_memory()
                .unwrap();
        let reg = ExtensionRegistry::new();
        let (ok, broken) = reg.load_from_store_with_version(dir.path(), &install, "0.1.0");
        assert_eq!(ok, 0);
        assert_eq!(broken, 1);
        let got = reg.get("needs-future").unwrap();
        assert_eq!(got.status, ExtensionStatus::Broken);
        assert!(got.broken_reason.as_deref().unwrap().contains("Vulcan ≥"));
    }

    #[test]
    fn load_from_store_marks_invalid_manifest_broken_and_records_error() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path().join("extensions/broken/extension.toml"),
            "completely[broken",
        );
        let install =
            crate::extensions::install_state::SqliteInstallStateStore::try_open_in_memory()
                .unwrap();
        // Pre-create a state row so record_load_error has somewhere
        // to land.
        crate::extensions::install_state::InstallStateStore::upsert(
            &install,
            &crate::extensions::install_state::InstallState {
                id: "broken".into(),
                version: "0.0.0".into(),
                enabled: false,
                installed_at: chrono::Utc::now(),
                last_load_error: None,
            },
        )
        .unwrap();
        let reg = ExtensionRegistry::new();
        let (ok, broken) = reg.load_from_store(dir.path(), &install);
        assert_eq!(ok, 0);
        assert_eq!(broken, 1);
        let got = reg.get("broken").unwrap();
        assert_eq!(got.status, ExtensionStatus::Broken);
        assert!(got.broken_reason.is_some());
        let state = crate::extensions::install_state::InstallStateStore::get(&install, "broken")
            .unwrap()
            .unwrap();
        assert!(state.last_load_error.is_some());
    }

    #[test]
    fn metadata_round_trips_through_serde_json() {
        let m = meta("alpha", 10);
        let json = serde_json::to_string(&m).unwrap();
        let back: ExtensionMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
    }
}
