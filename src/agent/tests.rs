use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::hooks::HookRegistry;
use crate::provider::mock::{MockProvider, MockResponse};
use crate::provider::{LLMProvider, Message};
use crate::skills::SkillRegistry;
use crate::tools::ToolRegistry;

use super::Agent;
use super::dispatch::{elided_lines, preview_output, summarize_tool_args, summarize_tool_result};
use super::run::empty_terminal_message;

fn empty_skills() -> Arc<SkillRegistry> {
    // Point at a path that doesn't exist so the registry is empty.
    Arc::new(SkillRegistry::new(&std::path::PathBuf::from(
        "/tmp/vulcan-test-skills-nonexistent",
    )))
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
            tx: tokio::sync::mpsc::UnboundedSender<crate::provider::StreamEvent>,
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
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let streamed = a2.run_prompt_stream("x", tx).await.unwrap();
    // Drain the channel.
    while let Ok(_) = rx.try_recv() {}

    assert_eq!(buffered, streamed);
    assert_eq!(buffered, "identical output");
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
async fn empty_terminal_response_after_tool_explains_why_buffered() {
    // YYC-104: agent used to return an empty string when the model
    // emitted no content + no tool calls after a tool error. That
    // exit looked silent to the caller. Now the run_prompt return
    // value carries a structured hint (iteration, tool-call count).
    let (mut agent, mock) = agent_with_mock();
    mock.enqueue_tool_call(
        "read_file",
        "call_1",
        serde_json::json!({"path": "/tmp/vulcan-test-nonexistent-file"}),
    );
    mock.enqueue_text(""); // model gives up after seeing tool error

    let resp = agent.run_prompt("read it").await.unwrap();
    assert!(
        resp.contains("1 tool call"),
        "expected tool count in hint, got {resp:?}"
    );
    assert!(
        resp.contains("iteration 1"),
        "expected iteration in hint, got {resp:?}"
    );
    assert!(resp.contains("terminal turn"), "got {resp:?}");
}

#[tokio::test]
async fn empty_terminal_response_after_tool_explains_why_streaming() {
    let (mut agent, mock) = agent_with_mock();
    mock.enqueue_tool_call(
        "read_file",
        "call_1",
        serde_json::json!({"path": "/tmp/vulcan-test-nonexistent-file"}),
    );
    mock.enqueue_text("");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let resp = agent.run_prompt_stream("read it", tx).await.unwrap();

    assert!(resp.contains("1 tool call"), "got {resp:?}");
    assert!(resp.contains("iteration 1"), "got {resp:?}");
    assert!(resp.contains("terminal turn"), "got {resp:?}");

    // The hint should also reach the TUI via a Text event.
    let mut saw_text_hint = false;
    while let Ok(ev) = rx.try_recv() {
        if let crate::provider::StreamEvent::Text(t) = ev {
            if t.contains("terminal turn") {
                saw_text_hint = true;
            }
        }
    }
    assert!(saw_text_hint, "TUI never saw the hint Text event");
}

#[test]
fn empty_terminal_message_includes_reasoning_and_context_hints() {
    use crate::provider::Usage;
    let usage = Usage {
        prompt_tokens: 970,
        completion_tokens: 0,
        total_tokens: 970,
    };
    // 970/1000 = 97% — over the 0.95 threshold, expect a hint.
    let msg = empty_terminal_message(3, 5, 1234, Some(&usage), 1_000);
    assert!(msg.contains("5 tool calls"), "got {msg:?}");
    assert!(msg.contains("iteration 3"), "got {msg:?}");
    assert!(msg.contains("1234 chars"), "got {msg:?}");
    assert!(msg.contains("near context limit"), "got {msg:?}");
    // Plural / singular agreement for tool count.
    let msg = empty_terminal_message(0, 1, 0, None, 0);
    assert!(msg.contains("1 tool call"), "got {msg:?}");
    assert!(!msg.contains("1 tool calls"), "got {msg:?}");
    // No reasoning, no usage → just the base summary.
    assert!(!msg.contains("chars"), "should hide reasoning hint when 0");
    assert!(!msg.contains("near context"), "should hide context hint when no usage");
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

    let mut agent = Agent::new(&config).await.unwrap();
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
            tx: tokio::sync::mpsc::UnboundedSender<crate::provider::StreamEvent>,
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
    // Three sequential 40ms sleeps would be ~120ms; parallel ≈ 40ms.
    // Allow generous slack for runtime jitter.
    assert!(
        elapsed < std::time::Duration::from_millis(110),
        "dispatch took {elapsed:?} — looks sequential"
    );
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
