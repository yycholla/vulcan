//! Core skills extension.
//!
//! This wraps the previous in-tree `SkillsHook` registration in the
//! daemon extension surface so built-in context is wired the same way
//! as other hook-providing extensions.

use std::sync::Arc;

use vulcan::extensions::api::{
    DaemonCodeExtension, ExtensionRegistration, SessionExtension, SessionExtensionCtx,
};
use vulcan::extensions::{
    ExtensionCapability, ExtensionMetadata, ExtensionSource, ExtensionStatus,
};
use vulcan::hooks::HookHandler;
use vulcan::hooks::skills::SkillsHook;
use vulcan::skills::SkillRegistry;
use vulcan_extension_macros::include_manifest;

pub struct CoreSkillsExtension;

impl DaemonCodeExtension for CoreSkillsExtension {
    fn metadata(&self) -> ExtensionMetadata {
        let manifest = include_manifest!();
        let mut m = ExtensionMetadata::new(
            manifest.id,
            "Core Skills",
            manifest.version,
            ExtensionSource::Builtin,
        );
        m.status = ExtensionStatus::Active;
        m.core = manifest.core;
        m.priority = 10;
        m.capabilities = vec![
            ExtensionCapability::HookHandler,
            ExtensionCapability::PromptInjection,
        ];
        m.description =
            "Injects the available skills catalog and lazily activated skill bodies.".to_string();
        m
    }

    fn instantiate(&self, ctx: SessionExtensionCtx) -> Arc<dyn SessionExtension> {
        Arc::new(CoreSkillsSession {
            registry: Arc::new(SkillRegistry::default_for(&ctx.skills_dir, Some(&ctx.cwd))),
        })
    }
}

struct CoreSkillsSession {
    registry: Arc<SkillRegistry>,
}

impl SessionExtension for CoreSkillsSession {
    fn hook_handlers(&self) -> Vec<Arc<dyn HookHandler>> {
        vec![Arc::new(SkillsHook::new(Arc::clone(&self.registry)))]
    }
}

inventory::submit! {
    ExtensionRegistration {
        register: || Arc::new(CoreSkillsExtension) as Arc<dyn DaemonCodeExtension>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_marks_extension_core() {
        let meta = CoreSkillsExtension.metadata();
        assert!(meta.core);
        assert_eq!(meta.priority, 10);
        assert_eq!(meta.status, ExtensionStatus::Active);
        assert!(
            meta.capabilities
                .contains(&ExtensionCapability::PromptInjection)
        );
    }
}
