//! YYC-193: agent contract tests — high-level behavioral
//! invariants asserted against a mocked agent loop. New
//! contracts live alongside the small harness in `harness.rs`
//! and follow the comments at the top of each test for the
//! recipe.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use vulcan::agent::Agent;
use vulcan::hooks::HookRegistry;
use vulcan::provider::mock::MockProvider;
use vulcan::provider::{LLMProvider, Message, StreamEvent, ToolDefinition};
use vulcan::skills::SkillRegistry;
use vulcan::tools::{ToolProfile, ToolRegistry, builtin_profile};

// ── Harness ───────────────────────────────────────────────────────

/// Wrap a `MockProvider` so we can reuse the queued-response API
/// from tests/agent_loop.rs without depending on its private
/// types.
struct ProviderHandle(Arc<MockProvider>);

#[async_trait]
impl LLMProvider for ProviderHandle {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<vulcan::provider::ChatResponse> {
        self.0.chat(messages, tools, cancel).await
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        tx: mpsc::Sender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Result<()> {
        self.0.chat_stream(messages, tools, tx, cancel).await
    }

    fn max_context(&self) -> usize {
        self.0.max_context()
    }
}

fn empty_skills() -> Arc<SkillRegistry> {
    Arc::new(SkillRegistry::new(&std::path::PathBuf::from(
        "/tmp/vulcan-contract-skills-nonexistent",
    )))
}

/// Build a fresh agent + mock provider pair with the given tool
/// profile applied. Pass `None` to leave the registry
/// unrestricted (the historical default).
fn agent_with_profile(profile: Option<ToolProfile>) -> (Agent, Arc<MockProvider>) {
    let mock = Arc::new(MockProvider::new(128_000));
    let mut tools = ToolRegistry::new();
    if let Some(p) = profile {
        tools.apply_profile(&p);
    }
    let agent = Agent::for_test(
        Box::new(ProviderHandle(mock.clone())),
        tools,
        HookRegistry::new(),
        empty_skills(),
    );
    (agent, mock)
}

fn tool_names(agent: &Agent) -> Vec<String> {
    agent
        .tool_definitions()
        .into_iter()
        .map(|d| d.function.name)
        .collect()
}

// ── Contracts ─────────────────────────────────────────────────────

/// Contract 1: a `readonly` profile must not expose any
/// workspace-mutating tools to the model. Catches regressions
/// where a future tool registration forgets to honor the active
/// profile.
#[tokio::test]
async fn readonly_profile_does_not_expose_mutating_tools() {
    let (agent, _mock) = agent_with_profile(builtin_profile("readonly"));
    let names = tool_names(&agent);
    for forbidden in [
        "write_file",
        "edit_file",
        "bash",
        "git_commit",
        "git_push",
        "spawn_subagent",
    ] {
        assert!(
            !names.contains(&forbidden.to_string()),
            "readonly profile leaked {forbidden:?} into tool defs: {names:?}"
        );
    }
}

/// Contract 2: a `gateway-safe` profile blocks not just shell
/// but also cargo and code-edit tools — so platform lanes can't
/// scribble on the workspace via a tool we forgot to consider.
#[tokio::test]
async fn gateway_safe_profile_blocks_all_workspace_mutation() {
    let (agent, _mock) = agent_with_profile(builtin_profile("gateway-safe"));
    let names = tool_names(&agent);
    for forbidden in [
        "write_file",
        "edit_file",
        "bash",
        "cargo_check",
        "git_commit",
        "git_push",
        "rename_symbol",
        "replace_function_body",
    ] {
        assert!(
            !names.contains(&forbidden.to_string()),
            "gateway-safe leaked {forbidden:?}: {names:?}"
        );
    }
}

/// Contract 3: an unrestricted agent (no profile) still
/// registers the foundational read-only tools. Guards against a
/// refactor accidentally dropping a tool from the default
/// registry.
#[tokio::test]
async fn default_registry_contains_foundational_tools() {
    let (agent, _mock) = agent_with_profile(None);
    let names = tool_names(&agent);
    for must in ["read_file", "list_files", "search_files", "git_status"] {
        assert!(
            names.contains(&must.to_string()),
            "default registry missing {must:?}: {names:?}"
        );
    }
}

/// Contract 4: when a profile-narrowed agent is asked to call a
/// disallowed tool, dispatch produces a structured denial that
/// the LLM can self-correct from — not a panic, not a silent
/// drop. The run record must reflect the failure.
#[tokio::test]
async fn disallowed_tool_call_produces_structured_denial_in_run_record() {
    let (mut agent, mock) = agent_with_profile(builtin_profile("readonly"));
    // Mock asks for write_file (disallowed under readonly), then
    // recovers with text on the next turn.
    mock.enqueue_tool_call(
        "write_file",
        "attempt_write",
        serde_json::json!({"path": "/tmp/x", "content": "y"}),
    );
    mock.enqueue_text("could not write — denied.");
    let _ = agent.run_prompt("write something").await.unwrap();

    let store = agent.run_store();
    let recent = store.recent(1).unwrap();
    let record = &recent[0];
    let tool_evs: Vec<(String, bool)> = record
        .events
        .iter()
        .filter_map(|e| match e {
            vulcan::run_record::RunEvent::ToolCall { name, is_error, .. } => {
                Some((name.clone(), *is_error))
            }
            _ => None,
        })
        .collect();
    assert_eq!(tool_evs.len(), 1);
    assert_eq!(tool_evs[0].0, "write_file");
    assert!(
        tool_evs[0].1,
        "disallowed tool call must be flagged is_error=true"
    );
}

/// Contract 5: tool errors are distinguishable from successful
/// tool calls in the run record. Pairs with contract 4 — same
/// shape, different cause (real tool, real failure mode).
#[tokio::test]
async fn tool_errors_are_distinguishable_from_successes() {
    let (mut agent, mock) = agent_with_profile(None);
    mock.enqueue_tool_call(
        "read_file",
        "read_missing",
        serde_json::json!({"path": "/this/does/not/exist/yyc-193"}),
    );
    mock.enqueue_text("could not read.");
    let _ = agent.run_prompt("try a missing path").await.unwrap();

    let store = agent.run_store();
    let recent = store.recent(1).unwrap();
    let record = &recent[0];
    let errs: Vec<bool> = record
        .events
        .iter()
        .filter_map(|e| match e {
            vulcan::run_record::RunEvent::ToolCall { is_error, .. } => Some(*is_error),
            _ => None,
        })
        .collect();
    assert_eq!(errs, vec![true]);
}

/// Contract 7: trust profile resolution falls through to a
/// conservative default when no rule matches. Pin the
/// resolver-side contract; downstream Agent integration is
/// covered separately.
#[tokio::test]
async fn trust_resolver_unknown_workspace_defaults_to_untrusted() {
    use std::path::PathBuf;
    use vulcan::trust::{TrustLevel, WorkspaceTrustConfig};

    let cfg = WorkspaceTrustConfig::default();
    // A path that doesn't exist on disk still resolves — the
    // resolver canonicalizes best-effort and falls back when no
    // rule matches.
    let nowhere = PathBuf::from("/tmp/yyc-182-contract-no-such-path");
    let p = cfg.resolve_for(&nowhere);
    assert_eq!(p.level, TrustLevel::Untrusted);
    assert_eq!(p.capability_profile, "readonly");
    assert!(!p.allow_indexing);
    assert!(!p.allow_persistence);
}

/// Contract 6: a successful turn produces no `ProviderError`
/// events. The negative-space contract — guards against future
/// refactors that swallow real provider errors but keep
/// emitting them on a happy turn.
#[tokio::test]
async fn happy_turn_produces_no_provider_error_events() {
    let (mut agent, mock) = agent_with_profile(None);
    mock.enqueue_text("plain reply.");
    let _ = agent.run_prompt("hi").await.unwrap();

    let store = agent.run_store();
    let record = &store.recent(1).unwrap()[0];
    let provider_errors = record
        .events
        .iter()
        .filter(|e| matches!(e, vulcan::run_record::RunEvent::ProviderError { .. }))
        .count();
    assert_eq!(
        provider_errors, 0,
        "happy turn must not emit ProviderError events: {:?}",
        record.events
    );
}
