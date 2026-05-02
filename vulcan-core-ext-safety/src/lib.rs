//! Core safety extension.
//!
//! This moves the dangerous-command safety gate onto the same daemon
//! extension registration path used by other hook-providing code.

use std::sync::Arc;

use vulcan::config::DangerousCommandPolicy;
use vulcan::extensions::api::{
    DaemonCodeExtension, ExtensionRegistration, SessionExtension, SessionExtensionCtx,
};
use vulcan::extensions::{
    ExtensionCapability, ExtensionMetadata, ExtensionSource, ExtensionStatus,
};
use vulcan::hooks::HookHandler;
use vulcan::hooks::safety::SafetyHook;
use vulcan_extension_macros::include_manifest;

pub struct CoreSafetyExtension;

impl DaemonCodeExtension for CoreSafetyExtension {
    fn metadata(&self) -> ExtensionMetadata {
        let manifest = include_manifest!();
        let mut m = ExtensionMetadata::new(
            manifest.id,
            "Core Safety",
            manifest.version,
            ExtensionSource::Builtin,
        );
        m.status = ExtensionStatus::Active;
        m.core = manifest.core;
        m.priority = 0;
        m.requires_user_approval = manifest.requires_user_approval;
        m.capabilities = vec![ExtensionCapability::HookHandler];
        m.description =
            "Blocks dangerous shell and PTY commands unless policy or HITL approval allows them."
                .to_string();
        m
    }

    fn instantiate(&self, ctx: SessionExtensionCtx) -> Arc<dyn SessionExtension> {
        Arc::new(CoreSafetySession { ctx })
    }
}

struct CoreSafetySession {
    ctx: SessionExtensionCtx,
}

impl SessionExtension for CoreSafetySession {
    fn hook_handlers(&self) -> Vec<Arc<dyn HookHandler>> {
        if matches!(
            self.ctx.dangerous_commands.policy,
            DangerousCommandPolicy::Allow
        ) {
            return Vec::new();
        }
        vec![Arc::new(SafetyHook::with_config(
            self.ctx.pause_tx.clone(),
            self.ctx.dangerous_commands,
        ))]
    }
}

inventory::submit! {
    ExtensionRegistration {
        register: || Arc::new(CoreSafetyExtension) as Arc<dyn DaemonCodeExtension>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_marks_extension_core_and_hitl() {
        let meta = CoreSafetyExtension.metadata();
        assert!(meta.core);
        assert_eq!(meta.priority, 0);
        assert_eq!(meta.status, ExtensionStatus::Active);
        assert!(meta.requires_user_approval);
    }
}
