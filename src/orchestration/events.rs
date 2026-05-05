//! Typed orchestration extension events.
//!
//! GH issue #271 is deliberately scoped to the typed runtime surfaces
//! that exist today. Vulcan does not yet own a typed plan/step runtime,
//! so this module adds delegation events around `spawn_subagent` and
//! leaves planning hooks to the future planner.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::ChildAgentId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationBudget {
    pub max_iterations: u32,
    pub token_budget: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationRequest {
    pub child_id: ChildAgentId,
    pub parent_session_id: Option<String>,
    pub task: String,
    pub allowed_tools: Vec<String>,
    pub profile_name: Option<String>,
    pub budget: DelegationBudget,
}

impl DelegationRequest {
    pub fn tool_count(&self) -> usize {
        self.allowed_tools.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegationStage {
    Requested,
    Started,
    Completed,
    Failed,
    Cancelled,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationEvent {
    pub request: DelegationRequest,
    pub stage: DelegationStage,
    pub iterations_used: u32,
    pub tokens_consumed: u64,
    pub summary: Option<String>,
    pub reason: Option<String>,
}

impl DelegationEvent {
    pub fn requested(request: DelegationRequest) -> Self {
        Self {
            request,
            stage: DelegationStage::Requested,
            iterations_used: 0,
            tokens_consumed: 0,
            summary: None,
            reason: None,
        }
    }

    pub fn started(request: DelegationRequest) -> Self {
        Self {
            request,
            stage: DelegationStage::Started,
            iterations_used: 0,
            tokens_consumed: 0,
            summary: None,
            reason: None,
        }
    }

    pub fn blocked(request: DelegationRequest, reason: impl Into<String>) -> Self {
        Self {
            request,
            stage: DelegationStage::Blocked,
            iterations_used: 0,
            tokens_consumed: 0,
            summary: None,
            reason: Some(reason.into()),
        }
    }

    pub fn terminal(
        request: DelegationRequest,
        stage: DelegationStage,
        iterations_used: u32,
        tokens_consumed: u64,
        summary: Option<String>,
        reason: Option<String>,
    ) -> Self {
        Self {
            request,
            stage,
            iterations_used,
            tokens_consumed,
            summary,
            reason,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegationDecision {
    Continue,
    Block { reason: String },
}

#[async_trait]
pub trait OrchestrationHook: Send + Sync {
    fn name(&self) -> &str;

    fn priority(&self) -> i32 {
        100
    }

    async fn before_delegation(
        &self,
        _request: &DelegationRequest,
        _cancel: CancellationToken,
    ) -> Result<DelegationDecision> {
        Ok(DelegationDecision::Continue)
    }

    async fn on_delegation_event(
        &self,
        _event: &DelegationEvent,
        _cancel: CancellationToken,
    ) -> Result<()> {
        Ok(())
    }
}

#[derive(Default, Clone)]
pub struct OrchestrationHookSet {
    hooks: Vec<Arc<dyn OrchestrationHook>>,
}

impl OrchestrationHookSet {
    pub fn new(mut hooks: Vec<Arc<dyn OrchestrationHook>>) -> Self {
        hooks.sort_by(|a, b| {
            a.priority()
                .cmp(&b.priority())
                .then_with(|| a.name().cmp(b.name()))
        });
        Self { hooks }
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    pub async fn before_delegation(
        &self,
        request: &DelegationRequest,
        cancel: CancellationToken,
    ) -> Result<DelegationDecision> {
        for hook in &self.hooks {
            match hook.before_delegation(request, cancel.clone()).await? {
                DelegationDecision::Continue => {}
                block @ DelegationDecision::Block { .. } => return Ok(block),
            }
        }
        Ok(DelegationDecision::Continue)
    }

    pub async fn emit_delegation(&self, event: &DelegationEvent, cancel: CancellationToken) {
        for hook in &self.hooks {
            if let Err(err) = hook.on_delegation_event(event, cancel.clone()).await {
                tracing::warn!(
                    hook = hook.name(),
                    error = %err,
                    "orchestration extension hook failed while observing delegation"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct TestHook {
        name: &'static str,
        priority: i32,
        decision: DelegationDecision,
    }

    #[async_trait]
    impl OrchestrationHook for TestHook {
        fn name(&self) -> &str {
            self.name
        }

        fn priority(&self) -> i32 {
            self.priority
        }

        async fn before_delegation(
            &self,
            _request: &DelegationRequest,
            _cancel: CancellationToken,
        ) -> Result<DelegationDecision> {
            Ok(self.decision.clone())
        }
    }

    fn request() -> DelegationRequest {
        DelegationRequest {
            child_id: ChildAgentId::new(),
            parent_session_id: Some("parent".into()),
            task: "inspect".into(),
            allowed_tools: vec!["read_file".into()],
            profile_name: None,
            budget: DelegationBudget {
                max_iterations: 3,
                token_budget: Some(100),
            },
        }
    }

    #[tokio::test]
    async fn blocking_hooks_are_priority_ordered_first_wins() {
        let hooks = OrchestrationHookSet::new(vec![
            Arc::new(TestHook {
                name: "later",
                priority: 20,
                decision: DelegationDecision::Block {
                    reason: "later block".into(),
                },
            }),
            Arc::new(TestHook {
                name: "continue",
                priority: 0,
                decision: DelegationDecision::Continue,
            }),
            Arc::new(TestHook {
                name: "earlier",
                priority: 10,
                decision: DelegationDecision::Block {
                    reason: "earlier block".into(),
                },
            }),
        ]);

        let decision = hooks
            .before_delegation(&request(), CancellationToken::new())
            .await
            .expect("hook dispatch ok");

        assert_eq!(
            decision,
            DelegationDecision::Block {
                reason: "earlier block".into()
            }
        );
    }
}
