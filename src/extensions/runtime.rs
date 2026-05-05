//! GH issue #273: extension runtime abstraction.
//!
//! This module is intentionally independent from manifest parsing and
//! from any concrete VM. It defines the host-visible lifecycle contract,
//! resource limits, capability audit records, and error taxonomy used by
//! sandboxed runtimes.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::policy::{ExtensionPermission, ExtensionPolicyEngine, PolicyDecision};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionRuntimeKind {
    Wasm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionRuntimeCapability {
    Log,
    RegisterTool,
    PersistentState,
    McpLaunch,
}

impl ExtensionRuntimeCapability {
    pub fn requested_permission(self) -> Option<ExtensionPermission> {
        match self {
            Self::Log => None,
            Self::RegisterTool => Some(ExtensionPermission::ToolRegistration),
            Self::PersistentState => Some(ExtensionPermission::PersistentState),
            Self::McpLaunch => Some(ExtensionPermission::McpLaunch),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExtensionRuntimeLimits {
    pub max_memory_bytes: usize,
    pub fuel: u64,
    pub call_timeout: Duration,
    pub max_host_call_depth: u32,
}

impl Default for ExtensionRuntimeLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: 16 * 1024 * 1024,
            fuel: 10_000_000,
            call_timeout: Duration::from_secs(2),
            max_host_call_depth: 8,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExtensionRuntimeCtx {
    pub extension_id: String,
    pub declared_permissions: BTreeSet<ExtensionPermission>,
    pub policy: Arc<ExtensionPolicyEngine>,
}

impl ExtensionRuntimeCtx {
    pub fn new(
        extension_id: impl Into<String>,
        declared_permissions: BTreeSet<ExtensionPermission>,
        policy: Arc<ExtensionPolicyEngine>,
    ) -> Self {
        Self {
            extension_id: extension_id.into(),
            declared_permissions,
            policy,
        }
    }

    pub fn decide(&self, capability: ExtensionRuntimeCapability) -> ExtensionRuntimeDecision {
        let Some(requested_permission) = capability.requested_permission() else {
            return ExtensionRuntimeDecision {
                extension_id: self.extension_id.clone(),
                capability,
                requested_permission: None,
                decision: PolicyDecision::Allow,
                allowed: true,
                failure_reason: None,
            };
        };
        let decision = self.policy.decide(
            &self.extension_id,
            &self.declared_permissions,
            requested_permission,
        );
        let allowed = decision.is_allow();
        let failure_reason = if allowed {
            None
        } else {
            Some(match &decision {
                PolicyDecision::Deny { reason }
                | PolicyDecision::RequireApproval { reason }
                | PolicyDecision::AllowWithRedaction { reason }
                | PolicyDecision::AllowWithQuota { reason, .. } => reason.clone(),
                PolicyDecision::Allow => String::new(),
            })
        };
        ExtensionRuntimeDecision {
            extension_id: self.extension_id.clone(),
            capability,
            requested_permission: Some(requested_permission),
            decision,
            allowed,
            failure_reason,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionRuntimeDecision {
    pub extension_id: String,
    pub capability: ExtensionRuntimeCapability,
    pub requested_permission: Option<ExtensionPermission>,
    pub decision: PolicyDecision,
    pub allowed: bool,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionRuntimeInit {
    pub extension_id: String,
    pub registered_tools: Vec<String>,
    pub decisions: Vec<ExtensionRuntimeDecision>,
}

#[derive(Debug, Error)]
pub enum ExtensionRuntimeError {
    #[error("runtime load failed for `{extension_id}`: {reason}")]
    LoadFailed {
        extension_id: String,
        reason: String,
    },
    #[error("runtime export missing for `{extension_id}`: {export}")]
    MissingExport {
        extension_id: String,
        export: &'static str,
    },
    #[error("runtime limit exceeded for `{extension_id}`: {limit}")]
    LimitExceeded { extension_id: String, limit: String },
    #[error("runtime capability denied for `{extension_id}`: {capability:?}: {reason}")]
    CapabilityDenied {
        extension_id: String,
        capability: ExtensionRuntimeCapability,
        reason: String,
    },
    #[error("runtime trapped for `{extension_id}`: {reason}")]
    Trap {
        extension_id: String,
        reason: String,
    },
}

impl ExtensionRuntimeError {
    pub fn extension_id(&self) -> &str {
        match self {
            Self::LoadFailed { extension_id, .. }
            | Self::MissingExport { extension_id, .. }
            | Self::LimitExceeded { extension_id, .. }
            | Self::CapabilityDenied { extension_id, .. }
            | Self::Trap { extension_id, .. } => extension_id,
        }
    }
}

#[async_trait::async_trait]
pub trait ExtensionRuntime: Send + Sync {
    fn kind(&self) -> ExtensionRuntimeKind;
    fn limits(&self) -> &ExtensionRuntimeLimits;
    async fn initialize(
        &self,
        ctx: ExtensionRuntimeCtx,
    ) -> Result<ExtensionRuntimeInit, ExtensionRuntimeError>;
    async fn shutdown(&self, _extension_id: &str) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_host_call_is_allowed_without_manifest_permission() {
        let ctx = ExtensionRuntimeCtx::new(
            "logger",
            BTreeSet::new(),
            Arc::new(ExtensionPolicyEngine::new()),
        );
        let decision = ctx.decide(ExtensionRuntimeCapability::Log);
        assert!(decision.allowed);
        assert!(decision.requested_permission.is_none());
    }

    #[test]
    fn tool_registration_requires_declared_permission() {
        let ctx = ExtensionRuntimeCtx::new(
            "tooler",
            BTreeSet::new(),
            Arc::new(ExtensionPolicyEngine::new()),
        );
        let decision = ctx.decide(ExtensionRuntimeCapability::RegisterTool);
        assert!(!decision.allowed);
        assert_eq!(
            decision.requested_permission,
            Some(ExtensionPermission::ToolRegistration)
        );
        assert!(matches!(decision.decision, PolicyDecision::Deny { .. }));
    }
}
