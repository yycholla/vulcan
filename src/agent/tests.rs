use super::dispatch::{elided_lines, preview_output, summarize_tool_args, summarize_tool_result};
use super::run::sanitize_orphan_tool_messages;
use super::turn::{TurnEvent, TurnMode, TurnRunnerMut, TurnStatus};
use super::*;
use crate::hooks::HookRegistry;
use crate::provider::StreamEvent;
use crate::provider::mock::{MockProvider, MockResponse};
use crate::skills::SkillRegistry;
use crate::tools::ToolRegistry;
use serde_json::Value;
use std::sync::Arc;

fn empty_skills() -> Arc<SkillRegistry> {
    Arc::new(SkillRegistry::empty())
}

fn asst_with_tool_calls(ids: &[&str]) -> Message {
    Message::Assistant {
        content: None,
        tool_calls: Some(
            ids.iter()
                .map(|id| crate::provider::ToolCall {
                    id: (*id).into(),
                    call_type: "function".into(),
                    function: crate::provider::ToolCallFunction {
                        name: "noop".into(),
                        arguments: "{}".into(),
                    },
                })
                .collect(),
        ),
        reasoning_content: None,
    }
}

fn tool_msg(id: &str) -> Message {
    Message::Tool {
        tool_call_id: id.into(),
        content: "ok".into(),
    }
}

#[test]
fn sanitize_drops_orphan_tool_with_no_preceding_assistant() {
    // YYC-138: Tool message with no Assistant tool_calls before it
    // is the failure mode the provider rejects with "Tool must
    // follow tool_calls". The sanitizer should drop it.
    let mut messages = vec![
        Message::User {
            content: "hello".into(),
        },
        tool_msg("orphan_id"),
        Message::User {
            content: "next".into(),
        },
    ];
    let dropped = sanitize_orphan_tool_messages(&mut messages);
    assert_eq!(dropped, 1);
    assert_eq!(messages.len(), 2);
    assert!(matches!(messages[0], Message::User { .. }));
    assert!(matches!(messages[1], Message::User { .. }));
}

#[test]
fn sanitize_keeps_tool_with_matching_assistant_tool_calls() {
    let mut messages = vec![
        Message::User {
            content: "go".into(),
        },
        asst_with_tool_calls(&["call_1"]),
        tool_msg("call_1"),
        Message::Assistant {
            content: Some("done".into()),
            tool_calls: None,
            reasoning_content: None,
        },
    ];
    let dropped = sanitize_orphan_tool_messages(&mut messages);
    assert_eq!(dropped, 0);
    assert_eq!(messages.len(), 4);
}

#[test]
fn sanitize_drops_tool_after_no_tool_calls_assistant() {
    // Asst without tool_calls should clear the active set, so a
    // following Tool with the previous turn's id is still an
    // orphan.
    let mut messages = vec![
        asst_with_tool_calls(&["call_1"]),
        tool_msg("call_1"),
        Message::Assistant {
            content: Some("text only".into()),
            tool_calls: None,
            reasoning_content: None,
        },
        tool_msg("call_1"),
    ];
    let dropped = sanitize_orphan_tool_messages(&mut messages);
    assert_eq!(dropped, 1);
    assert!(matches!(messages.last(), Some(Message::Assistant { .. })));
}

#[test]
fn sanitize_treats_empty_tool_calls_as_no_calls() {
    // Some providers return tool_calls: [] when the model meant to
    // emit none. Same effect on the wire as None — Tool messages
    // following are orphans.
    let mut messages = vec![
        Message::Assistant {
            content: None,
            tool_calls: Some(vec![]),
            reasoning_content: None,
        },
        tool_msg("call_1"),
    ];
    let dropped = sanitize_orphan_tool_messages(&mut messages);
    assert_eq!(dropped, 1);
    assert_eq!(messages.len(), 1);
}

#[test]
fn sanitize_handles_multiple_tool_calls_in_one_assistant_turn() {
    let mut messages = vec![
        asst_with_tool_calls(&["a", "b", "c"]),
        tool_msg("a"),
        tool_msg("c"),
        tool_msg("b"),
        tool_msg("d"), // orphan — not in the active set
    ];
    let dropped = sanitize_orphan_tool_messages(&mut messages);
    assert_eq!(dropped, 1);
    // a, c, b survive (order preserved); d dropped.
    assert_eq!(messages.len(), 4);
}

/// Build an Agent with a MockProvider and minimal setup. Returns the agent
/// and a handle to the mock so tests can enqueue responses + inspect calls.
fn agent_with_mock() -> (Agent, Arc<MockProvider>) {
    let mock = Arc::new(MockProvider::new(128_000));
    // The agent needs Box<dyn LLMProvider>; we wrap a clone of the Arc.
    // Since MockProvider's state is in interior Mutex, cloning the Arc
    // gives the test a handle to the same instance.
    struct ProviderHandle(Arc<MockProvider>);
    #[async_trait::async_trait]
    impl LLMProvider for ProviderHandle {
        async fn chat(
            &self,
            m: &[Message],
            t: &[crate::provider::ToolDefinition],
            c: CancellationToken,
        ) -> Result<crate::provider::ChatResponse> {
            self.0.chat(m, t, c).await
        }
        async fn chat_stream(
            &self,
            m: &[Message],
            t: &[crate::provider::ToolDefinition],
            tx: tokio::sync::mpsc::Sender<crate::provider::StreamEvent>,
            c: CancellationToken,
        ) -> Result<()> {
            self.0.chat_stream(m, t, tx, c).await
        }
        fn max_context(&self) -> usize {
            self.0.max_context()
        }
    }
    let agent = Agent::for_test(
        Box::new(ProviderHandle(mock.clone())),
        ToolRegistry::new(),
        HookRegistry::new(),
        empty_skills(),
    );
    (agent, mock)
}

fn agent_with_mock_and_hooks(hooks: HookRegistry) -> (Agent, Arc<MockProvider>) {
    let mock = Arc::new(MockProvider::new(128_000));
    struct ProviderHandle(Arc<MockProvider>);
    #[async_trait::async_trait]
    impl LLMProvider for ProviderHandle {
        async fn chat(
            &self,
            m: &[Message],
            t: &[crate::provider::ToolDefinition],
            c: CancellationToken,
        ) -> Result<crate::provider::ChatResponse> {
            self.0.chat(m, t, c).await
        }
        async fn chat_stream(
            &self,
            m: &[Message],
            t: &[crate::provider::ToolDefinition],
            tx: tokio::sync::mpsc::Sender<crate::provider::StreamEvent>,
            c: CancellationToken,
        ) -> Result<()> {
            self.0.chat_stream(m, t, tx, c).await
        }
        fn max_context(&self) -> usize {
            self.0.max_context()
        }
    }
    let agent = Agent::for_test(
        Box::new(ProviderHandle(mock.clone())),
        ToolRegistry::new(),
        hooks,
        empty_skills(),
    );
    (agent, mock)
}

#[tokio::test]
async fn builder_accepts_hooks_pause_channel_and_max_iterations() {
    let mut config = Config::default();
    config.provider.base_url = "http://127.0.0.1:11434/v1".into();
    config.provider.disable_catalog = true;
    config.provider.max_iterations = 12;
    let (pause_tx, _pause_rx) = crate::pause::channel(1);

    let agent = Agent::builder(&config)
        .with_hooks(HookRegistry::new())
        .with_pause_channel(pause_tx)
        .with_max_iterations(3)
        .build()
        .await
        .unwrap();

    assert_eq!(agent.max_iterations, 3);
    assert!(
        agent
            .tools
            .definitions()
            .iter()
            .any(|tool| tool.function.name == "ask_user")
    );
}

#[tokio::test]
async fn single_turn_text_response() {
    let (mut agent, mock) = agent_with_mock();
    mock.enqueue_text("Hello there");

    let resp = agent.run_prompt("hi").await.unwrap();
    assert_eq!(resp, "Hello there");

    // Provider was called once; messages had system + user (no history).
    let calls = mock.captured_calls();
    assert_eq!(calls.len(), 1);
    assert!(matches!(calls[0][0], Message::System { .. }));
    match &calls[0][1] {
        Message::User { content } => assert_eq!(content, "hi"),
        other => panic!("expected User, got {other:?}"),
    }
}

#[tokio::test]
async fn multi_turn_with_tool_call() {
    let (mut agent, mock) = agent_with_mock();
    // Iter 0: tool call. Iter 1: final text response.
    mock.enqueue_tool_call(
        "read_file",
        "call_1",
        serde_json::json!({"path": "/tmp/vulcan-test-nonexistent-file"}),
    );
    mock.enqueue_text("Read failed but that's fine for the test");

    // The real ReadFile tool is registered by ToolRegistry::new(); it'll
    // return Err for the bogus path, which dispatch_tool wraps as
    // ToolResult::err. The agent's iteration 1 sees a Tool message with
    // the error string and emits the queued text response.
    let resp = agent.run_prompt("read it").await.unwrap();
    assert_eq!(resp, "Read failed but that's fine for the test");

    let calls = mock.captured_calls();
    assert_eq!(
        calls.len(),
        2,
        "should call provider twice (tool, then final)"
    );

    // Iteration 1's messages should include the tool result.
    let iter1 = &calls[1];
    assert!(
        iter1.iter().any(|m| matches!(m, Message::Tool { .. })),
        "iteration 1 should include a Tool message in history"
    );
}

#[tokio::test]
async fn streaming_and_buffered_paths_match() {
    // Same scripted response in both paths; final returned text should match.
    let (mut a1, m1) = agent_with_mock();
    m1.enqueue_text("identical output");
    let buffered = a1.run_prompt("x").await.unwrap();

    let (mut a2, m2) = agent_with_mock();
    m2.enqueue_text("identical output");
    let (tx, mut rx) = tokio::sync::mpsc::channel(crate::provider::STREAM_CHANNEL_CAPACITY);
    let streamed = a2.run_prompt_stream("x", tx).await.unwrap();
    // Drain the channel.
    while rx.try_recv().is_ok() {}

    assert_eq!(buffered, streamed);
    assert_eq!(buffered, "identical output");
}

#[tokio::test]
async fn run_prompt_stream_emits_one_terminal_done() {
    let (mut agent, mock) = agent_with_mock();
    mock.enqueue_text("streamed once");
    let (tx, mut rx) = tokio::sync::mpsc::channel(crate::provider::STREAM_CHANNEL_CAPACITY);

    let streamed = agent.run_prompt_stream("x", tx).await.unwrap();

    let mut done_count = 0;
    let mut text = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            StreamEvent::Text(chunk) => text.push_str(&chunk),
            StreamEvent::Done(resp) => {
                done_count += 1;
                assert_eq!(resp.content.as_deref(), Some("streamed once"));
            }
            other => panic!("unexpected stream event: {other:?}"),
        }
    }

    assert_eq!(streamed, "streamed once");
    assert_eq!(text, "streamed once");
    assert_eq!(done_count, 1, "streaming adapter must emit one Done");
}

#[tokio::test]
async fn prepare_stream_turn_builds_prompt_and_persists_user_message() {
    let (mut agent, _mock) = agent_with_mock();
    let cancel = CancellationToken::new();

    let turn = agent
        .prepare_stream_turn("stream this", cancel.clone())
        .await
        .unwrap();

    cancel.cancel();
    assert!(agent.turn_cancel.is_cancelled());
    assert!(matches!(turn.messages[0], Message::System { .. }));
    assert!(matches!(
        turn.messages.last(),
        Some(Message::User { content }) if content == "stream this"
    ));
    assert_eq!(agent.last_saved_count, turn.messages.len());
    assert!(!turn.tool_defs.is_empty());
}

#[tokio::test]
async fn prepare_turn_builds_prompt_and_persists_user_message() {
    let (mut agent, _mock) = agent_with_mock();
    let cancel = CancellationToken::new();

    let turn = agent
        .prepare_turn("turn this", cancel.clone())
        .await
        .unwrap();

    cancel.cancel();
    assert!(agent.turn_cancel.is_cancelled());
    assert!(matches!(turn.messages[0], Message::System { .. }));
    assert!(matches!(
        turn.messages.last(),
        Some(Message::User { content }) if content == "turn this"
    ));
    assert_eq!(agent.last_saved_count, turn.messages.len());
    assert!(!turn.tool_defs.is_empty());
}

#[tokio::test]
async fn compact_stream_messages_if_needed_replaces_history_with_summary_and_keeps_recent_window() {
    // YYC-128: real compaction calls the provider to summarize the older
    // slice, splices the summary in place of it, and preserves the recent
    // window verbatim — including the new user prompt.
    let (mut agent, mock) = agent_with_mock();
    agent.context = ContextManager::new(10);
    // Summarizer call returns this body — the new System message should
    // contain it and not the legacy "Previous conversation context:" stub.
    mock.enqueue_text("- user wanted X done\n- file /tmp/foo.txt was created");

    let (tx, mut rx) = tokio::sync::mpsc::channel(crate::provider::STREAM_CHANNEL_CAPACITY);
    let mut messages = vec![Message::System {
        content: "system".into(),
    }];
    // Build 8 (User, Assistant) turns so the keep_recent=6 window leaves
    // older content to summarize.
    for i in 0..8 {
        messages.push(Message::User {
            content: format!("turn {i} user"),
        });
        messages.push(Message::Assistant {
            content: Some(format!("turn {i} answer")),
            tool_calls: None,
            reasoning_content: None,
        });
    }
    messages.push(Message::User {
        content: "new prompt".into(),
    });
    let pre_len = messages.len();

    agent
        .compact_stream_messages_if_needed(&mut messages, "new prompt", &tx, 0)
        .await;

    // History was actually rewritten.
    assert!(messages.len() < pre_len);
    // First message stays the original System prompt.
    assert!(matches!(&messages[0], Message::System { content } if content == "system"));
    // Second message is the inserted summary System block.
    match &messages[1] {
        Message::System { content } => {
            assert!(content.starts_with("Summary of earlier conversation:"));
            assert!(content.contains("/tmp/foo.txt"));
        }
        other => panic!("expected summary System, got {other:?}"),
    }
    // Recent window preserved verbatim — the new user prompt is still last.
    assert!(matches!(
        messages.last(),
        Some(Message::User { content }) if content == "new prompt"
    ));
    // UX note emitted to the stream.
    assert!(matches!(
        rx.try_recv(),
        Ok(StreamEvent::Text(note)) if note.contains("compacted")
    ));
}

#[tokio::test]
async fn compact_turn_messages_if_needed_emits_domain_event() {
    let (mut agent, mock) = agent_with_mock();
    agent.context = ContextManager::new(10);
    mock.enqueue_text("- user wanted X done\n- file /tmp/foo.txt was created");

    let (tx, mut rx) = tokio::sync::mpsc::channel(crate::provider::STREAM_CHANNEL_CAPACITY);
    let mut messages = vec![Message::System {
        content: "system".into(),
    }];
    for i in 0..8 {
        messages.push(Message::User {
            content: format!("turn {i} user"),
        });
        messages.push(Message::Assistant {
            content: Some(format!("turn {i} answer")),
            tool_calls: None,
            reasoning_content: None,
        });
    }
    messages.push(Message::User {
        content: "new prompt".into(),
    });

    agent
        .compact_turn_messages_if_needed(&mut messages, &tx, 0)
        .await;

    assert!(matches!(
        rx.try_recv(),
        Ok(TurnEvent::Compacted { earlier_messages }) if earlier_messages > 0
    ));
}

#[tokio::test]
async fn rewrite_history_from_before_compact_replaces_durable_history() {
    struct RewriteHook;

    #[async_trait::async_trait]
    impl crate::hooks::HookHandler for RewriteHook {
        fn name(&self) -> &str {
            "compact-summary"
        }

        async fn on_session_before_compact(
            &self,
            _messages: &[Message],
            _cancel: CancellationToken,
        ) -> Result<crate::hooks::HookOutcome> {
            Ok(crate::hooks::HookOutcome::RewriteHistory(vec![
                Message::System {
                    content: "extension summary".into(),
                },
            ]))
        }
    }

    let hooks = HookRegistry::new();
    hooks.register(Arc::new(RewriteHook));
    let (mut agent, mock) = agent_with_mock_and_hooks(hooks);
    agent.context = ContextManager::new(10);

    let (tx, mut rx) = tokio::sync::mpsc::channel(crate::provider::STREAM_CHANNEL_CAPACITY);
    let mut messages = vec![
        Message::System {
            content: "system".into(),
        },
        Message::User {
            content: "old ".repeat(200),
        },
    ];

    agent
        .compact_turn_messages_if_needed(&mut messages, &tx, 0)
        .await;

    assert_eq!(messages.len(), 1);
    assert!(matches!(&messages[0], Message::System { content } if content == "extension summary"));
    assert!(
        mock.captured_calls().is_empty(),
        "valid rewrite must bypass built-in summarizer"
    );
    assert!(matches!(
        rx.try_recv(),
        Ok(TurnEvent::Compacted { earlier_messages }) if earlier_messages > 0
    ));
}

#[tokio::test]
async fn invalid_rewrite_history_falls_back_to_builtin_and_audits_rejection() {
    use crate::extensions::{CompactionAuditAction, ExtensionAuditEvent, ExtensionAuditLog};

    struct BadRewriteHook;

    #[async_trait::async_trait]
    impl crate::hooks::HookHandler for BadRewriteHook {
        fn name(&self) -> &str {
            "bad-compact"
        }

        async fn on_session_before_compact(
            &self,
            _messages: &[Message],
            _cancel: CancellationToken,
        ) -> Result<crate::hooks::HookOutcome> {
            Ok(crate::hooks::HookOutcome::RewriteHistory(vec![
                Message::User {
                    content: "missing system".into(),
                },
            ]))
        }
    }

    let audit = Arc::new(ExtensionAuditLog::new(8));
    let hooks = HookRegistry::new().with_audit_log(audit.clone());
    hooks.register(Arc::new(BadRewriteHook));
    let (mut agent, mock) = agent_with_mock_and_hooks(hooks);
    agent.context = ContextManager::new(10);
    mock.enqueue_text("built in summary");

    let (tx, _rx) = tokio::sync::mpsc::channel(crate::provider::STREAM_CHANNEL_CAPACITY);
    let mut messages = vec![Message::System {
        content: "system".into(),
    }];
    for i in 0..8 {
        messages.push(Message::User {
            content: format!("old {i} {}", "x ".repeat(40)),
        });
        messages.push(Message::Assistant {
            content: Some(format!("answer {i}")),
            tool_calls: None,
            reasoning_content: None,
        });
    }
    messages.push(Message::User {
        content: "fresh".into(),
    });

    agent
        .compact_turn_messages_if_needed(&mut messages, &tx, 0)
        .await;

    assert!(
        messages.iter().any(
            |m| matches!(m, Message::System { content } if content.contains("built in summary"))
        )
    );
    assert!(audit.recent(8).iter().any(|event| matches!(
        event,
        ExtensionAuditEvent::Compaction(compaction)
            if compaction.extension_id == "bad-compact"
                && matches!(compaction.action, CompactionAuditAction::ValidationFailed { .. })
    )));
}

#[tokio::test]
async fn block_skips_compaction_until_context_overflow_is_imminent() {
    struct BlockHook;

    #[async_trait::async_trait]
    impl crate::hooks::HookHandler for BlockHook {
        fn name(&self) -> &str {
            "block-compact"
        }

        async fn on_session_before_compact(
            &self,
            _messages: &[Message],
            _cancel: CancellationToken,
        ) -> Result<crate::hooks::HookOutcome> {
            Ok(crate::hooks::HookOutcome::Block {
                reason: "not now".into(),
            })
        }
    }

    let hooks = HookRegistry::new();
    hooks.register(Arc::new(BlockHook));
    let (mut agent, mock) = agent_with_mock_and_hooks(hooks);
    agent.context = ContextManager::with_config(
        10_000,
        crate::config::CompactionConfig {
            enabled: true,
            trigger_ratio: 0.01,
            reserved_tokens: 0,
        },
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel(crate::provider::STREAM_CHANNEL_CAPACITY);
    let original = vec![
        Message::System {
            content: "system".into(),
        },
        Message::User {
            content: "old ".repeat(200),
        },
    ];
    let mut messages = original.clone();

    agent
        .compact_turn_messages_if_needed(&mut messages, &tx, 0)
        .await;

    assert_eq!(messages.len(), original.len());
    assert!(mock.captured_calls().is_empty());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn block_is_overridden_on_context_overflow_and_audited() {
    use crate::extensions::{CompactionAuditAction, ExtensionAuditEvent, ExtensionAuditLog};

    struct BlockHook;

    #[async_trait::async_trait]
    impl crate::hooks::HookHandler for BlockHook {
        fn name(&self) -> &str {
            "block-compact"
        }

        async fn on_session_before_compact(
            &self,
            _messages: &[Message],
            _cancel: CancellationToken,
        ) -> Result<crate::hooks::HookOutcome> {
            Ok(crate::hooks::HookOutcome::Block {
                reason: "not now".into(),
            })
        }
    }

    let audit = Arc::new(ExtensionAuditLog::new(8));
    let hooks = HookRegistry::new().with_audit_log(audit.clone());
    hooks.register(Arc::new(BlockHook));
    let (mut agent, mock) = agent_with_mock_and_hooks(hooks);
    agent.context = ContextManager::new(10);
    mock.enqueue_text("forced summary");

    let (tx, mut rx) = tokio::sync::mpsc::channel(crate::provider::STREAM_CHANNEL_CAPACITY);
    let mut messages = vec![Message::System {
        content: "system".into(),
    }];
    for i in 0..8 {
        messages.push(Message::User {
            content: format!("old {i} {}", "x ".repeat(40)),
        });
        messages.push(Message::Assistant {
            content: Some(format!("answer {i}")),
            tool_calls: None,
            reasoning_content: None,
        });
    }
    messages.push(Message::User {
        content: "fresh".into(),
    });

    agent
        .compact_turn_messages_if_needed(&mut messages, &tx, 0)
        .await;

    assert!(matches!(
        rx.try_recv(),
        Ok(TurnEvent::CompactionForced { extension_id, reason })
            if extension_id == "block-compact" && reason == "not now"
    ));
    assert!(
        messages.iter().any(
            |m| matches!(m, Message::System { content } if content.contains("forced summary"))
        )
    );
    assert!(audit.recent(8).iter().any(|event| matches!(
        event,
        ExtensionAuditEvent::Compaction(compaction)
            if compaction.extension_id == "block-compact"
                && matches!(compaction.action, CompactionAuditAction::Forced { .. })
    )));
}

#[tokio::test]
async fn compact_skips_when_no_user_boundary_in_recent_window() {
    // No User in the trailing window (mid tool-loop): compaction must be a
    // no-op so we don't break the tool_calls/Tool wire invariant.
    let (mut agent, _mock) = agent_with_mock();
    agent.context = ContextManager::new(10);
    let (tx, _rx) = tokio::sync::mpsc::channel(crate::provider::STREAM_CHANNEL_CAPACITY);
    let original = vec![
        Message::System {
            content: "system".into(),
        },
        Message::Assistant {
            content: Some("a".repeat(200)),
            tool_calls: None,
            reasoning_content: None,
        },
        Message::Assistant {
            content: Some("b".repeat(200)),
            tool_calls: None,
            reasoning_content: None,
        },
    ];
    let mut messages = original.clone();

    agent
        .compact_stream_messages_if_needed(&mut messages, "x", &tx, 0)
        .await;

    assert_eq!(messages.len(), original.len());
}

#[tokio::test]
async fn collect_stream_response_forwards_text_and_returns_done() {
    let (agent, mock) = agent_with_mock();
    mock.enqueue_text("streamed text");
    let (tx, mut rx) = tokio::sync::mpsc::channel(crate::provider::STREAM_CHANNEL_CAPACITY);
    let messages = vec![Message::User {
        content: "x".into(),
    }];

    let response = agent
        .collect_stream_response(&messages, &[], &tx, CancellationToken::new(), 0)
        .await
        .unwrap();

    assert_eq!(response.content.as_deref(), Some("streamed text"));
    assert!(matches!(
        rx.try_recv(),
        Ok(StreamEvent::Text(text)) if text == "streamed text"
    ));
    assert!(
        rx.try_recv().is_err(),
        "provider Done stays private; the agent loop emits the final UI Done after turn finalization"
    );
}

#[tokio::test]
async fn collect_turn_response_emits_domain_events() {
    let (agent, mock) = agent_with_mock();
    mock.enqueue_text("streamed text");
    let (tx, mut rx) = tokio::sync::mpsc::channel(crate::provider::STREAM_CHANNEL_CAPACITY);
    let messages = vec![Message::User {
        content: "x".into(),
    }];

    let response = agent
        .collect_turn_response(&messages, &[], &tx, CancellationToken::new(), 0)
        .await
        .unwrap();

    assert_eq!(response.content.as_deref(), Some("streamed text"));
    assert!(matches!(
        rx.try_recv(),
        Ok(TurnEvent::Text { text }) if text == "streamed text"
    ));
    assert!(matches!(
        rx.try_recv(),
        Ok(TurnEvent::ProviderDone { response }) if response.content.as_deref() == Some("streamed text")
    ));
}

#[tokio::test]
async fn execute_stream_tool_calls_emits_events_and_preserves_result_order() {
    let (agent, _mock) = agent_with_mock();
    let calls = vec![
        crate::provider::ToolCall {
            id: "call_a".into(),
            call_type: "function".into(),
            function: crate::provider::ToolCallFunction {
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "/tmp/nope-a"}).to_string(),
            },
        },
        crate::provider::ToolCall {
            id: "call_b".into(),
            call_type: "function".into(),
            function: crate::provider::ToolCallFunction {
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "/tmp/nope-b"}).to_string(),
            },
        },
    ];
    let (tx, mut rx) = tokio::sync::mpsc::channel(crate::provider::STREAM_CHANNEL_CAPACITY);

    let results = agent
        .execute_stream_tool_calls(&calls, &tx, CancellationToken::new())
        .await;

    assert_eq!(
        results
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["call_a", "call_b"]
    );
    let mut starts = 0;
    let mut ends = 0;
    while let Ok(event) = rx.try_recv() {
        match event {
            StreamEvent::ToolCallStart { .. } => starts += 1,
            StreamEvent::ToolCallEnd { .. } => ends += 1,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert_eq!(starts, 2);
    assert_eq!(ends, 2);
}

#[tokio::test]
async fn run_prompt_with_cancel_origin_stamps_subagent_run_record() {
    // Slice 7: child runs land in the run-record store with
    // `RunOrigin::Subagent { parent_run_id }`, so `vulcan run show`
    // and analytics queries can discover parent → child lineage
    // without joining against orchestration metadata.
    use crate::run_record::{RunId, RunOrigin};
    let (mut agent, mock) = agent_with_mock();
    mock.enqueue_text("done");
    let parent_run_id = RunId::new();

    let cancel = CancellationToken::new();
    let _text = agent
        .run_prompt_with_cancel_origin("child task", cancel, RunOrigin::Subagent { parent_run_id })
        .await
        .unwrap();

    let recent = agent.run_store().recent(10).expect("list recent runs");
    let subagent_run = recent
        .iter()
        .find(|r| matches!(r.origin, RunOrigin::Subagent { .. }))
        .expect("subagent run present in store");
    match &subagent_run.origin {
        RunOrigin::Subagent {
            parent_run_id: prid,
        } => assert_eq!(prid, &parent_run_id),
        other => panic!("expected Subagent origin, got {other:?}"),
    }
}

#[tokio::test]
async fn cache_matches_persisted_state_after_completed_turn() {
    // Slice 2 acceptance: cancelled (or simply finished) turns leave
    // history valid for the next provider request — that means the
    // in-memory cache must not drift from durable storage.
    let (mut agent, mock) = agent_with_mock();
    mock.enqueue_text("done");
    agent.run_prompt("hi").await.unwrap();

    let stored = agent
        .memory
        .load_history(agent.session_id())
        .unwrap()
        .unwrap_or_default();
    assert_eq!(
        stored.len(),
        agent.history_cache.len(),
        "cache and storage must agree on message count after a turn"
    );
    for (idx, (cache_msg, store_msg)) in agent.history_cache.iter().zip(stored.iter()).enumerate() {
        assert!(
            std::mem::discriminant(cache_msg) == std::mem::discriminant(store_msg),
            "message {idx} discriminant differs between cache and storage"
        );
    }
}

#[tokio::test]
async fn compaction_updates_in_memory_history_cache() {
    // Slice 2 acceptance: compaction routes through one adapter that
    // updates both durable storage and the live cache, so the next
    // prepare_turn sees the summarized snapshot — not a stale Vec full
    // of pre-compaction turns.
    let (mut agent, mock) = agent_with_mock();
    agent.context = ContextManager::new(10);
    mock.enqueue_text("- summary line one\n- summary line two");

    let mut messages = vec![Message::System {
        content: "system".into(),
    }];
    for i in 0..6 {
        messages.push(Message::User {
            content: format!("user {i}"),
        });
        messages.push(Message::Assistant {
            content: Some(format!("assistant {i}")),
            tool_calls: None,
            reasoning_content: None,
        });
    }
    messages.push(Message::User {
        content: "fresh".into(),
    });

    let pre_len = messages.len();
    let cancel = CancellationToken::new();
    agent.turn_cancel = cancel.clone();
    let did_compact = agent
        .compact_buffered_messages_if_possible(&mut messages, cancel)
        .await;
    assert!(did_compact, "compaction must rewrite buffer for this test");
    assert!(messages.len() < pre_len);

    // The in-memory cache must mirror the post-compaction conversation
    // (everything past the leading System frame).
    let expected_cache: Vec<_> = messages.iter().skip(1).cloned().collect();
    assert_eq!(agent.history_cache.len(), expected_cache.len());
    assert!(
        agent.history_loaded,
        "cache must be marked loaded after replace_history"
    );
}

#[tokio::test]
async fn agent_built_with_pool_shares_pool_session_store() {
    // Slice 3 acceptance: sessions assemble from pool adapters instead
    // of opening their own SessionStore. Writing through the agent's
    // memory must be visible from the pool's handle.
    let mut config = Config::default();
    config.provider.base_url = "http://127.0.0.1:11434/v1".into();
    config.provider.disable_catalog = true;
    let pool = Arc::new(crate::runtime_pool::RuntimeResourcePool::for_tests());

    let agent = Agent::builder(&config)
        .with_pool(Arc::clone(&pool))
        .build()
        .await
        .unwrap();

    let session_id = agent.session_id().to_string();
    agent
        .memory
        .save_messages(
            &session_id,
            &[Message::User {
                content: "shared write".into(),
            }],
        )
        .unwrap();

    // Same SessionStore: pool's handle observes the write.
    let read_back = pool.session_store().load_history(&session_id).unwrap();
    let messages = read_back.expect("session present");
    assert!(matches!(
        messages.first(),
        Some(Message::User { content }) if content == "shared write"
    ));
}

#[tokio::test]
async fn second_prepare_turn_uses_cached_history_not_storage_reload() {
    // Slice 2 acceptance: in-memory SessionHistory is canonical; storage
    // is durability + recovery only. Wiping storage between turns must
    // not erase the live transcript a session is reasoning over.
    let (mut agent, mock) = agent_with_mock();
    mock.enqueue_text("first answer");
    agent.run_prompt("hello").await.unwrap();

    // Simulate a storage corruption / external wipe between turns. With a
    // cached SessionHistory, the next prepare_turn still sees prior
    // conversation; without one, history would silently reset.
    agent.memory.save_messages(agent.session_id(), &[]).unwrap();

    let cancel = CancellationToken::new();
    let turn = agent.prepare_turn("again", cancel).await.unwrap();

    let saw_user_hello = turn
        .messages
        .iter()
        .any(|m| matches!(m, Message::User { content } if content == "hello"));
    let saw_assistant_first = turn.messages.iter().any(|m| {
        matches!(
            m,
            Message::Assistant { content: Some(c), .. } if c == "first answer"
        )
    });
    assert!(
        saw_user_hello,
        "cached history must include first user turn"
    );
    assert!(
        saw_assistant_first,
        "cached history must include first assistant turn"
    );
}

#[tokio::test]
async fn turn_runner_run_buffered_completes_single_iteration() {
    // Slice 1 acceptance: buffered execution flows through TurnRunner.run,
    // returning a TurnOutcome instead of duplicating the iteration loop.
    let (mut agent, mock) = agent_with_mock();
    mock.enqueue_text("hello");
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel(crate::provider::STREAM_CHANNEL_CAPACITY);

    let outcome = TurnRunnerMut::new(&mut agent)
        .run("hi", cancel, &tx, TurnMode::Buffered)
        .await
        .unwrap();
    drop(tx);
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.final_text, "hello");
    assert!(matches!(outcome.status, TurnStatus::Completed));
    let response = outcome
        .final_response
        .expect("buffered run yields response");
    assert_eq!(response.content.as_deref(), Some("hello"));
}

#[tokio::test]
async fn turn_runner_run_streaming_emits_text_and_completes() {
    // Slice 1 acceptance: streaming execution shares the same loop, emitting
    // text tokens through the TurnEvent sink.
    let (mut agent, mock) = agent_with_mock();
    mock.enqueue_text("streamed");
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel(crate::provider::STREAM_CHANNEL_CAPACITY);

    let outcome = TurnRunnerMut::new(&mut agent)
        .run("hi", cancel, &tx, TurnMode::Streaming)
        .await
        .unwrap();
    drop(tx);

    let mut got_text = false;
    while let Some(event) = rx.recv().await {
        if let TurnEvent::Text { text } = &event {
            if text == "streamed" {
                got_text = true;
            }
        }
    }
    assert!(got_text, "streaming sink should receive text event");
    assert_eq!(outcome.final_text, "streamed");
    assert!(matches!(outcome.status, TurnStatus::Completed));
}

#[tokio::test]
async fn turn_runner_run_handles_tool_call_then_terminal_text() {
    let (mut agent, mock) = agent_with_mock();
    mock.enqueue_tool_call(
        "read_file",
        "call_x",
        serde_json::json!({"path": "/tmp/vulcan-runner-nope"}),
    );
    mock.enqueue_text("done");

    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel(crate::provider::STREAM_CHANNEL_CAPACITY);
    let outcome = TurnRunnerMut::new(&mut agent)
        .run("read it", cancel, &tx, TurnMode::Buffered)
        .await
        .unwrap();
    drop(tx);
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.final_text, "done");
    assert!(matches!(outcome.status, TurnStatus::Completed));
    let calls = mock.captured_calls();
    assert_eq!(calls.len(), 2);
    assert!(
        calls[1].iter().any(|m| matches!(m, Message::Tool { .. })),
        "tool message must reach the second iteration"
    );
}

#[tokio::test]
async fn execute_turn_tool_calls_emits_domain_events_and_preserves_result_order() {
    let (agent, _mock) = agent_with_mock();
    let calls = vec![
        crate::provider::ToolCall {
            id: "call_a".into(),
            call_type: "function".into(),
            function: crate::provider::ToolCallFunction {
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "/tmp/nope-a"}).to_string(),
            },
        },
        crate::provider::ToolCall {
            id: "call_b".into(),
            call_type: "function".into(),
            function: crate::provider::ToolCallFunction {
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "/tmp/nope-b"}).to_string(),
            },
        },
    ];
    let (tx, mut rx) = tokio::sync::mpsc::channel(crate::provider::STREAM_CHANNEL_CAPACITY);

    let results = agent
        .execute_turn_tool_calls(&calls, &tx, CancellationToken::new())
        .await;

    assert_eq!(
        results
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["call_a", "call_b"]
    );
    let mut starts = 0;
    let mut ends = 0;
    while let Ok(event) = rx.try_recv() {
        match event {
            TurnEvent::ToolCallStart { .. } => starts += 1,
            TurnEvent::ToolCallEnd { .. } => ends += 1,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert_eq!(starts, 2);
    assert_eq!(ends, 2);
}

#[tokio::test]
async fn provider_error_propagates() {
    let (mut agent, mock) = agent_with_mock();
    mock.enqueue_error("simulated 500");

    let result = agent.run_prompt("anything").await;
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("simulated 500"), "got {msg:?}");
}

#[tokio::test]
async fn reasoning_carries_into_assistant_message() {
    let (mut agent, mock) = agent_with_mock();
    mock.enqueue(MockResponse::WithReasoning {
        reasoning: "the user wants a greeting".into(),
        content: "Hi!".into(),
    });

    let resp = agent.run_prompt("hello").await.unwrap();
    assert_eq!(resp, "Hi!");
    // run_prompt's final save_messages would have stored the reasoning;
    // not asserting against the DB to avoid touching ~/.vulcan in tests.
}

#[tokio::test]
async fn switch_model_rebuilds_provider_metadata_without_restarting_session() {
    let base_url = spawn_model_catalog_server().await;
    let config = Config {
        provider: crate::config::ProviderConfig {
            base_url,
            api_key: Some("test-key".into()),
            model: "model-a".into(),
            catalog_cache_ttl_hours: 0,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut agent = Agent::builder(&config).build().await.unwrap();
    let session_id = agent.session_id().to_string();

    assert_eq!(agent.active_model(), "model-a");
    assert_eq!(agent.max_context(), 1_000);
    assert_eq!(agent.pricing().map(|p| p.input_per_token), Some(0.000001));

    let selection = agent.switch_model("model-b").await.unwrap();

    assert_eq!(agent.session_id(), session_id);
    assert_eq!(selection.model.id, "model-b");
    assert_eq!(agent.active_model(), "model-b");
    assert_eq!(agent.max_context(), 2_000);
    assert_eq!(agent.pricing().map(|p| p.output_per_token), Some(0.000004));
}

#[tokio::test]
async fn switch_provider_model_uses_selected_model_before_catalog_validation() {
    let base_url = spawn_model_catalog_server().await;
    let mut providers = std::collections::HashMap::new();
    providers.insert(
        "qwen".into(),
        crate::config::ProviderConfig {
            base_url: base_url.clone(),
            api_key: Some("test-key".into()),
            model: "missing-model".into(),
            catalog_cache_ttl_hours: 0,
            ..Default::default()
        },
    );
    let config = Config {
        provider: crate::config::ProviderConfig {
            base_url,
            api_key: Some("test-key".into()),
            model: "model-a".into(),
            catalog_cache_ttl_hours: 0,
            ..Default::default()
        },
        providers,
        ..Default::default()
    };

    let mut agent = Agent::builder(&config).build().await.unwrap();
    let stale_profile = agent.switch_provider(Some("qwen"), &config).await;
    assert!(
        stale_profile.is_err(),
        "plain provider switch should still validate configured profile model"
    );

    let selection = agent
        .switch_provider_model(Some("qwen"), &config, "model-b")
        .await
        .unwrap();

    assert_eq!(selection.model.id, "model-b");
    assert_eq!(agent.active_profile(), Some("qwen"));
    assert_eq!(agent.active_model(), "model-b");
    assert_eq!(agent.max_context(), 2_000);
}

async fn spawn_model_catalog_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test server");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let body = r#"{
                    "data": [
                        {
                            "id": "model-a",
                            "context_length": 1000,
                            "pricing": {"prompt": "0.000001", "completion": "0.000002"},
                            "supported_parameters": ["tools", "response_format"]
                        },
                        {
                            "id": "model-b",
                            "context_length": 2000,
                            "pricing": {"prompt": "0.000003", "completion": "0.000004"},
                            "supported_parameters": ["tools"]
                        }
                    ]
                }"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/v1")
}

#[tokio::test]
async fn fork_session_records_lineage_and_switches_active_session() {
    let (mut agent, _mock) = agent_with_mock();
    let parent_id = agent.session_id().to_string();

    let child_id = agent.fork_session(Some("branched for UI work")).unwrap();

    assert_eq!(agent.session_id(), child_id);
    let summaries = agent.memory().list_sessions(10).unwrap();
    let child = summaries
        .iter()
        .find(|s| s.id == child_id)
        .expect("child summary should exist");
    assert_eq!(child.parent_session_id.as_deref(), Some(parent_id.as_str()));
    assert_eq!(child.lineage_label.as_deref(), Some("branched for UI work"));
}

#[tokio::test]
async fn fork_session_with_hooks_emits_before_fork_event() {
    struct ForkHook(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    #[async_trait::async_trait]
    impl crate::hooks::HookHandler for ForkHook {
        fn name(&self) -> &str {
            "fork-hook"
        }

        async fn on_session_before_fork(
            &self,
            _cancel: CancellationToken,
        ) -> Result<crate::hooks::HookOutcome> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(crate::hooks::HookOutcome::Continue)
        }
    }

    let mock = Arc::new(MockProvider::new(128_000));
    struct ProviderHandle(Arc<MockProvider>);
    #[async_trait::async_trait]
    impl LLMProvider for ProviderHandle {
        async fn chat(
            &self,
            m: &[Message],
            t: &[crate::provider::ToolDefinition],
            c: CancellationToken,
        ) -> Result<crate::provider::ChatResponse> {
            self.0.chat(m, t, c).await
        }

        async fn chat_stream(
            &self,
            m: &[Message],
            t: &[crate::provider::ToolDefinition],
            tx: tokio::sync::mpsc::Sender<crate::provider::StreamEvent>,
            c: CancellationToken,
        ) -> Result<()> {
            self.0.chat_stream(m, t, tx, c).await
        }

        fn max_context(&self) -> usize {
            self.0.max_context()
        }
    }

    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hooks = HookRegistry::new();
    hooks.register(Arc::new(ForkHook(calls.clone())));
    let mut agent = Agent::for_test(
        Box::new(ProviderHandle(mock)),
        ToolRegistry::new(),
        hooks,
        empty_skills(),
    );

    let child_id = agent
        .fork_session_with_hooks(Some("hooked fork"))
        .await
        .unwrap();

    assert_eq!(agent.session_id(), child_id);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// Tool that increments an in-flight counter, sleeps, then decrements.
/// Records the maximum observed concurrency so the test can assert that
/// parallel dispatch actually overlaps tool execution (YYC-34).
struct ConcurrencyProbeTool {
    in_flight: Arc<std::sync::atomic::AtomicUsize>,
    max_observed: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::tools::Tool for ConcurrencyProbeTool {
    fn name(&self) -> &str {
        "concurrency_probe"
    }
    fn description(&self) -> &str {
        "test tool that sleeps and tracks in-flight concurrency"
    }
    fn schema(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    async fn call(
        &self,
        _params: Value,
        _cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<crate::tools::ToolResult> {
        use std::sync::atomic::Ordering;
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_observed.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        Ok(crate::tools::ToolResult::ok("done"))
    }
}

#[tokio::test]
async fn parallel_tool_calls_dispatch_concurrently() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_observed = Arc::new(AtomicUsize::new(0));

    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(ConcurrencyProbeTool {
        in_flight: in_flight.clone(),
        max_observed: max_observed.clone(),
    }));

    let mock = Arc::new(MockProvider::new(128_000));
    struct ProviderHandle(Arc<MockProvider>);
    #[async_trait::async_trait]
    impl LLMProvider for ProviderHandle {
        async fn chat(
            &self,
            m: &[Message],
            t: &[crate::provider::ToolDefinition],
            c: CancellationToken,
        ) -> Result<crate::provider::ChatResponse> {
            self.0.chat(m, t, c).await
        }
        async fn chat_stream(
            &self,
            m: &[Message],
            t: &[crate::provider::ToolDefinition],
            tx: tokio::sync::mpsc::Sender<crate::provider::StreamEvent>,
            c: CancellationToken,
        ) -> Result<()> {
            self.0.chat_stream(m, t, tx, c).await
        }
        fn max_context(&self) -> usize {
            self.0.max_context()
        }
    }
    let mut agent = Agent::for_test(
        Box::new(ProviderHandle(mock.clone())),
        tools,
        HookRegistry::new(),
        empty_skills(),
    );

    // Iter 0: three parallel calls. Iter 1: final text.
    mock.enqueue_tool_calls(vec![
        ("concurrency_probe", "call_a", serde_json::json!({})),
        ("concurrency_probe", "call_b", serde_json::json!({})),
        ("concurrency_probe", "call_c", serde_json::json!({})),
    ]);
    mock.enqueue_text("done");

    let started = std::time::Instant::now();
    let resp = agent.run_prompt("go").await.unwrap();
    let elapsed = started.elapsed();

    assert_eq!(resp, "done");
    // Drop the wall-clock assertion: CI runners under load make
    // tokio scheduling jitter dominate the per-tool sleep, so the
    // sequential-vs-parallel timing comparison is brittle. The
    // `max_observed >= 2` invariant below is a sufficient proof of
    // concurrent dispatch — it counts simultaneous in-flight
    // futures, which is the property the test actually wants to pin.
    let _ = elapsed;
    assert!(
        max_observed.load(Ordering::SeqCst) >= 2,
        "expected ≥2 concurrent dispatches, observed {}",
        max_observed.load(Ordering::SeqCst)
    );

    // Order preservation: tool messages line up with original call ids.
    let calls = mock.captured_calls();
    let iter1 = &calls[1];
    let tool_ids: Vec<&str> = iter1
        .iter()
        .filter_map(|m| match m {
            Message::Tool { tool_call_id, .. } => Some(tool_call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(tool_ids, vec!["call_a", "call_b", "call_c"]);
}

#[test]
fn summarize_tool_args_picks_meaningful_field_per_tool() {
    // YYC-74 — the YYC-74 card needs a one-line projection.
    assert_eq!(
        summarize_tool_args("read_file", r#"{"path":"src/foo.rs"}"#).as_deref(),
        Some("src/foo.rs")
    );
    assert_eq!(
        summarize_tool_args("git_commit", r#"{"message":"YYC-74"}"#).as_deref(),
        Some("YYC-74")
    );
    assert_eq!(
        summarize_tool_args("git_branch", r#"{"action":"create","name":"foo"}"#).as_deref(),
        Some("create foo")
    );
    // Long path tail-truncates rather than head-truncates.
    let long_path = "/very/long/leading/path/segments/that/blow/the/budget/file.rs";
    let result = summarize_tool_args("read_file", &format!(r#"{{"path":"{long_path}"}}"#)).unwrap();
    assert!(result.starts_with('…'));
    assert!(result.ends_with("file.rs"), "got {result}");
    // Generic fallback for unknown tools surfaces first string field.
    assert_eq!(
        summarize_tool_args("custom_tool", r#"{"x":42,"label":"hello"}"#).as_deref(),
        Some("hello")
    );
}

// YYC-210: per-tool summarizers for new LSP + subagent tools.
#[test]
fn summarize_tool_args_handles_new_tools() {
    assert_eq!(
        summarize_tool_args(
            "workspace_symbol",
            r#"{"query":"parse_config","language":"rust"}"#
        )
        .as_deref(),
        Some("parse_config [rust]"),
    );
    assert_eq!(
        summarize_tool_args(
            "type_definition",
            r#"{"path":"src/foo.rs","line":12,"character":4}"#
        )
        .as_deref(),
        Some("src/foo.rs:12"),
    );
    assert_eq!(
        summarize_tool_args(
            "implementation",
            r#"{"path":"src/foo.rs","line":42,"character":0}"#
        )
        .as_deref(),
        Some("src/foo.rs:42"),
    );
    assert_eq!(
        summarize_tool_args(
            "call_hierarchy",
            r#"{"path":"src/foo.rs","line":7,"character":2,"direction":"outgoing"}"#
        )
        .as_deref(),
        Some("src/foo.rs:7 (outgoing)"),
    );
    assert_eq!(
        summarize_tool_args("code_action", r#"{"path":"src/foo.rs","start_line":3}"#).as_deref(),
        Some("src/foo.rs:3"),
    );
    assert_eq!(
        summarize_tool_args(
            "spawn_subagent",
            r#"{"task":"Review the provider streaming parser"}"#
        )
        .as_deref(),
        Some("Review the provider streaming parser"),
    );
}

#[test]
fn summarize_tool_result_handles_new_tools() {
    assert_eq!(
        summarize_tool_result(
            "workspace_symbol",
            r#"{"query":"x","language":"rust","count":3,"hits":[]}"#
        )
        .as_deref(),
        Some("3 symbols"),
    );
    assert_eq!(
        summarize_tool_result(
            "type_definition",
            r#"{"locations":[{"uri":"file:///x"},{"uri":"file:///y"}]}"#
        )
        .as_deref(),
        Some("2 hits"),
    );
    assert_eq!(
        summarize_tool_result(
            "implementation",
            r#"{"implementations":[{"uri":"file:///x"}]}"#
        )
        .as_deref(),
        Some("1 hit"),
    );
    assert_eq!(
        summarize_tool_result(
            "call_hierarchy",
            r#"{"direction":"incoming","count":4,"calls":[]}"#
        )
        .as_deref(),
        Some("4 incoming calls"),
    );
    assert_eq!(
        summarize_tool_result(
            "code_action",
            r#"{"path":"src/foo.rs","count":2,"actions":[]}"#
        )
        .as_deref(),
        Some("2 actions"),
    );
    assert_eq!(
        summarize_tool_result(
            "spawn_subagent",
            r#"{"status":"completed","summary":"ok","budget_used":{"iterations":3,"max_iterations":8}}"#
        )
        .as_deref(),
        Some("completed · 3/8 iters"),
    );
}

#[test]
fn preview_output_caps_to_twelve_lines_and_one_kb() {
    // YYC-78 raised the cap so collapsed cards still show useful
    // context up front.
    let big = (1..=40)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let preview = preview_output(&big).unwrap();
    assert_eq!(preview.lines().count(), 12);
    assert!(preview.contains("line 1"));
    assert!(!preview.contains("line 13"));
}

#[test]
fn elided_lines_counts_what_was_clipped() {
    let big = (1..=40)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let preview = preview_output(&big);
    let elided = elided_lines(&big, preview.as_deref());
    assert_eq!(elided, 28);
    // Short output → no elision.
    let short = "one\ntwo\nthree";
    let preview = preview_output(short);
    assert_eq!(elided_lines(short, preview.as_deref()), 0);
}

#[test]
fn preview_output_returns_none_for_empty() {
    assert!(preview_output("").is_none());
    assert!(preview_output("   \n  ").is_none());
}

#[test]
fn summarize_tool_result_per_tool_meta() {
    // YYC-74: meta sub-header in the card.
    assert_eq!(
        summarize_tool_result("write_file", "Wrote 4321 bytes to /tmp/x").as_deref(),
        Some("4.2 KB written")
    );
    assert_eq!(
        summarize_tool_result("edit_file", "Replaced 3 occurrence(s) in /tmp/x").as_deref(),
        Some("3 occurrences")
    );
    assert_eq!(
        summarize_tool_result("git_status", "## main\n M src/foo.rs\n?? new.rs").as_deref(),
        Some("2 changes")
    );
    assert_eq!(
        summarize_tool_result("git_status", "## main").as_deref(),
        Some("clean")
    );
    assert_eq!(
        summarize_tool_result("git_diff", "+++ a\n+ added\n--- b\n- removed\n- removed2")
            .as_deref(),
        Some("+1 -2")
    );
    // Generic fallback (unknown tool) gets line/byte count.
    let s = summarize_tool_result("unknown_tool", "line one\nline two\nline three").unwrap();
    assert!(s.starts_with("3 lines"), "got {s}");
}

// YYC-152: is_local_base_url contract tests. Cover scheme/no-scheme,
// IPv4 + IPv6 forms, RFC1918 + link-local + .local, and the
// previously-broken edge cases (IPv6 brackets, IPv4-mapped IPv6).
#[test]
fn is_local_base_url_accepts_localhost_forms() {
    assert!(is_local_base_url("http://localhost:11434/v1"));
    assert!(is_local_base_url("https://LOCALHOST:8080"));
    assert!(is_local_base_url("localhost:11434"));
    assert!(is_local_base_url("http://my-host.local"));
    assert!(is_local_base_url("http://laptop.LOCAL"));
}

#[test]
fn is_local_base_url_accepts_ipv4_loopback_and_private() {
    assert!(is_local_base_url("http://127.0.0.1:11434"));
    assert!(is_local_base_url("http://0.0.0.0:8080"));
    assert!(is_local_base_url("http://10.0.0.5"));
    assert!(is_local_base_url("http://192.168.1.5"));
    assert!(is_local_base_url("http://172.16.0.1"));
    assert!(is_local_base_url("http://172.31.255.254"));
    assert!(is_local_base_url("http://169.254.1.1"));
}

#[test]
fn is_local_base_url_rejects_public_ipv4() {
    assert!(!is_local_base_url("http://8.8.8.8"));
    assert!(!is_local_base_url("https://api.example.com"));
    assert!(!is_local_base_url("http://172.32.0.1")); // outside 172.16/12
    assert!(!is_local_base_url("http://192.169.0.1")); // outside 192.168/16
}

#[test]
fn is_local_base_url_handles_ipv6_brackets() {
    // Previously the hand-rolled parser tripped on the bracket
    // form when a port was present (YYC-152).
    assert!(is_local_base_url("http://[::1]:11434"));
    assert!(is_local_base_url("http://[::1]"));
    assert!(is_local_base_url("http://[fc00::1]:8080")); // ULA
    assert!(!is_local_base_url("http://[2001:4860:4860::8888]"));
}

#[test]
fn is_local_base_url_handles_ipv4_mapped_ipv6() {
    // ::ffff:127.0.0.1 must classify as local because it routes
    // to a loopback IPv4. Previously the hand-rolled parser
    // rejected this form.
    assert!(is_local_base_url("http://[::ffff:127.0.0.1]:11434"));
    assert!(is_local_base_url("http://[::ffff:192.168.1.5]"));
    assert!(!is_local_base_url("http://[::ffff:8.8.8.8]"));
}

#[test]
fn is_local_base_url_returns_false_on_malformed_input() {
    assert!(!is_local_base_url(""));
    assert!(!is_local_base_url("not a url"));
    assert!(!is_local_base_url("http://"));
}

// YYC-249: Agent.provider_api_key must be wrapped in SecretString so
// the buffer is zeroed on drop and Debug doesn't leak the value.
#[test]
fn provider_api_key_is_secret_string() {
    let _: fn(&Agent) -> &secrecy::SecretString = |a| &a.provider_api_key;
}

#[test]
fn provider_api_key_debug_does_not_leak_value() {
    use secrecy::SecretString;
    let secret = SecretString::from("super-secret-key-do-not-leak".to_string());
    let dbg = format!("{secret:?}");
    assert!(
        !dbg.contains("super-secret-key-do-not-leak"),
        "SecretString Debug leaked the value: {dbg}"
    );
}
