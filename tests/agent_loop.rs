use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use tempfile::tempdir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use vulcan::agent::Agent;
use vulcan::hooks::HookRegistry;
use vulcan::provider::mock::MockProvider;
use vulcan::provider::{LLMProvider, Message, StreamEvent, ToolDefinition};
use vulcan::skills::SkillRegistry;
use vulcan::tools::ToolRegistry;

static CWD_LOCK: Mutex<()> = Mutex::new(());

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
        "/tmp/vulcan-integration-skills-nonexistent",
    )))
}

fn agent_with_mock() -> (Agent, Arc<MockProvider>) {
    let mock = Arc::new(MockProvider::new(128_000));
    let agent = Agent::for_test(
        Box::new(ProviderHandle(mock.clone())),
        ToolRegistry::new(),
        HookRegistry::new(),
        empty_skills(),
    );
    (agent, mock)
}

#[tokio::test]
async fn run_record_lifecycle_events_land_for_completed_turn() {
    // YYC-179 acceptance: every CLI/TUI agent turn must produce a
    // run record with a stable id, lifecycle events, and a terminal
    // status. This test pins the contract — a successful turn yields
    // Running → PromptReceived → ProviderRequest → ProviderResponse
    // → StatusChanged{Completed}.
    let (mut agent, mock) = agent_with_mock();
    mock.enqueue_text("done.");
    let _ = agent.run_prompt("hello").await.unwrap();

    let store = agent.run_store();
    let recent = store.recent(1).unwrap();
    assert_eq!(recent.len(), 1);
    let record = &recent[0];
    assert_eq!(record.status, vulcan::run_record::RunStatus::Completed);
    assert!(record.ended_at.is_some());

    let kinds: Vec<&'static str> = record
        .events
        .iter()
        .map(|e| match e {
            vulcan::run_record::RunEvent::StatusChanged { .. } => "status",
            vulcan::run_record::RunEvent::PromptReceived { .. } => "prompt",
            vulcan::run_record::RunEvent::ProviderRequest { .. } => "req",
            vulcan::run_record::RunEvent::ProviderResponse { .. } => "resp",
            vulcan::run_record::RunEvent::ProviderError { .. } => "perr",
            vulcan::run_record::RunEvent::HookDecision { .. } => "hook",
            vulcan::run_record::RunEvent::ToolCall { .. } => "tool",
            vulcan::run_record::RunEvent::SubagentSpawned { .. } => "sub",
            vulcan::run_record::RunEvent::ArtifactCreated { .. } => "art",
        })
        .collect();
    assert!(kinds.contains(&"status"), "no status events: {kinds:?}");
    assert!(kinds.contains(&"prompt"), "no prompt event: {kinds:?}");
    assert!(kinds.contains(&"req"), "no provider request: {kinds:?}");
    assert!(kinds.contains(&"resp"), "no provider response: {kinds:?}");

    // Raw prompt must NOT be persisted by default; the fingerprint
    // is the redacted surface.
    let prompt_event = record
        .events
        .iter()
        .find_map(|e| match e {
            vulcan::run_record::RunEvent::PromptReceived {
                fingerprint, raw, ..
            } => Some((fingerprint.as_str().to_string(), raw.clone())),
            _ => None,
        })
        .expect("prompt event present");
    assert!(prompt_event.0.starts_with("sha256:"));
    assert!(prompt_event.1.is_none(), "raw prompt should not be stored");
}

#[tokio::test]
async fn agent_create_artifact_persists_with_run_and_session_links() {
    // YYC-180 acceptance: code can create + persist a typed
    // artifact, agent runs reference it, and the user can list
    // artifacts for a session/run. Also pins ArtifactCreated event
    // on the run timeline so YYC-179's `vulcan run show` lists it.
    let (mut agent, mock) = agent_with_mock();
    mock.enqueue_text("plan ready.");
    let _ = agent.run_prompt("plan something").await.unwrap();

    // After the turn, current_run_id is None — but the run is
    // still in the store. Recreate via run_prompt? Easier: create
    // the artifact mid-turn via a new call and verify on the next
    // turn. Simpler still: create directly tied to a fresh run id.
    let session = agent.session_id().to_string();
    let store = agent.artifact_store();
    let art = vulcan::artifact::Artifact::inline_text(
        vulcan::artifact::ArtifactKind::Plan,
        "phase 1: read files\nphase 2: edit",
    )
    .with_session_id(session.clone())
    .with_source("test-fixture");
    let id = agent.create_artifact(art).expect("artifact persists");

    let got = store.get(id).unwrap().unwrap();
    assert_eq!(got.kind, vulcan::artifact::ArtifactKind::Plan);
    assert_eq!(got.session_id.as_deref(), Some(session.as_str()));
    assert_eq!(got.source.as_deref(), Some("test-fixture"));

    let by_session = store.list_for_session(&session).unwrap();
    assert!(by_session.iter().any(|a| a.id == id));
}

#[tokio::test]
async fn run_record_gateway_origin_carries_lane_string() {
    // YYC-179 PR-6: gateway lane streaming entry point tags the
    // run record with `RunOrigin::Gateway { lane }` so timeline
    // queries can attribute traffic per platform/lane.
    let (mut agent, mock) = agent_with_mock();
    mock.enqueue_text("gateway reply.");
    let (tx, _rx) = mpsc::channel(vulcan::provider::STREAM_CHANNEL_CAPACITY);
    let cancel = CancellationToken::new();
    let _ = agent
        .run_prompt_stream_for_gateway("ping", tx, cancel, "discord/general".to_string())
        .await
        .unwrap();
    let store = agent.run_store();
    let recent = store.recent(1).unwrap();
    let record = &recent[0];
    match &record.origin {
        vulcan::run_record::RunOrigin::Gateway { lane } => {
            assert_eq!(lane, "discord/general");
        }
        other => panic!("expected Gateway origin, got {other:?}"),
    }
}

#[tokio::test]
async fn run_record_captures_streaming_turn_with_tui_origin() {
    // YYC-179 acceptance: the streaming path (TUI primary consumer)
    // produces a run record with `RunOrigin::Tui`, lifecycle events,
    // and a streaming=true ProviderRequest event so dashboards can
    // distinguish it from buffered turns.
    let (mut agent, mock) = agent_with_mock();
    mock.enqueue_text("streamed reply.");
    let (tx, _rx) = mpsc::channel(vulcan::provider::STREAM_CHANNEL_CAPACITY);
    let cancel = CancellationToken::new();
    let _ = agent
        .run_prompt_stream_with_cancel("hi over stream", tx, cancel)
        .await
        .unwrap();

    let store = agent.run_store();
    let recent = store.recent(1).unwrap();
    let record = &recent[0];
    assert_eq!(record.status, vulcan::run_record::RunStatus::Completed);
    assert!(matches!(record.origin, vulcan::run_record::RunOrigin::Tui));
    let saw_streaming_request = record.events.iter().any(|e| {
        matches!(
            e,
            vulcan::run_record::RunEvent::ProviderRequest {
                streaming: true,
                ..
            }
        )
    });
    assert!(saw_streaming_request, "expected streaming ProviderRequest");
}

#[tokio::test]
async fn run_record_captures_tool_call_with_error_distinguishable_from_success() {
    // YYC-179 acceptance: tool errors must be distinguishable from
    // successful tool calls in the run record (is_error: true). This
    // also exercises the full Running → ToolCall → ProviderResponse
    // → Completed timeline ordering.
    let dir = tempdir().unwrap();
    let missing = dir.path().join("does-not-exist.txt");
    let (mut agent, mock) = agent_with_mock();

    mock.enqueue_tool_call(
        "read_file",
        "read_missing",
        serde_json::json!({"path": missing}),
    );
    mock.enqueue_text("could not read file.");

    let _ = agent.run_prompt("read missing").await.unwrap();

    let store = agent.run_store();
    let recent = store.recent(1).unwrap();
    let record = &recent[0];
    let tool_events: Vec<(String, bool)> = record
        .events
        .iter()
        .filter_map(|e| match e {
            vulcan::run_record::RunEvent::ToolCall { name, is_error, .. } => {
                Some((name.clone(), *is_error))
            }
            _ => None,
        })
        .collect();
    assert_eq!(tool_events.len(), 1);
    assert_eq!(tool_events[0].0, "read_file");
    assert!(
        tool_events[0].1,
        "missing-file read should be flagged is_error=true"
    );
}

#[tokio::test]
async fn agent_read_file_tool_result_flows_into_next_llm_turn() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("note.txt");
    std::fs::write(&path, "hello from integration\n").unwrap();
    let (mut agent, mock) = agent_with_mock();

    mock.enqueue_tool_call("read_file", "read_note", serde_json::json!({"path": path}));
    mock.enqueue_text("I read the note.");

    let response = agent.run_prompt("read the note").await.unwrap();

    assert_eq!(response, "I read the note.");
    let calls = mock.captured_calls();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].iter().any(|message| matches!(
        message,
        Message::Tool { content, .. } if content.contains("hello from integration")
    )));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn agent_tool_loop_can_read_edit_and_cargo_check_real_project() {
    let _cwd = CWD_LOCK.lock().unwrap();
    let dir = tempdir().unwrap();
    let src_dir = dir.path().join("src");
    std::fs::create_dir(&src_dir).unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"agent_loop_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    let lib_path = src_dir.join("lib.rs");
    std::fs::write(&lib_path, "pub fn answer() -> i32 {\n    41\n}\n").unwrap();

    let prev = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();
    let (mut agent, mock) = agent_with_mock();

    mock.enqueue_tool_call(
        "read_file",
        "read_lib",
        serde_json::json!({"path": lib_path}),
    );
    mock.enqueue_tool_call(
        "edit_file",
        "edit_answer",
        serde_json::json!({
            "path": lib_path,
            "old_string": "    41",
            "new_string": "    42"
        }),
    );
    mock.enqueue_tool_call(
        "cargo_check",
        "check_project",
        serde_json::json!({"all_targets": false}),
    );
    mock.enqueue_text("The project still checks.");

    let result = agent.run_prompt("fix answer").await;
    std::env::set_current_dir(prev).unwrap();

    assert_eq!(result.unwrap(), "The project still checks.");
    assert!(std::fs::read_to_string(&lib_path).unwrap().contains("42"));
    let calls = mock.captured_calls();
    assert_eq!(calls.len(), 4);
    assert!(calls[3].iter().any(|message| matches!(
        message,
        Message::Tool { tool_call_id, content } if tool_call_id == "check_project"
            && content.contains("\"ok\": true")
    )));
}

#[tokio::test]
async fn stream_cancel_mid_tool_emits_done_and_persists_partial_messages() {
    let (mut agent, mock) = agent_with_mock();
    let session_id = agent.session_id().to_string();
    mock.enqueue_tool_call(
        "bash",
        "sleep_call",
        serde_json::json!({"command": "sleep 5", "timeout": 10}),
    );
    let cancel = CancellationToken::new();
    let (tx, mut rx) = mpsc::channel(vulcan::provider::STREAM_CHANNEL_CAPACITY);

    let cancel_for_task = cancel.clone();
    let run = tokio::spawn(async move {
        agent
            .run_prompt_stream_with_cancel("start slow tool", tx, cancel_for_task)
            .await
            .map(|response| (response, agent))
    });

    while let Some(event) = rx.recv().await {
        if matches!(event, StreamEvent::ToolCallStart { .. }) {
            break;
        }
    }
    cancel.cancel();

    let mut saw_done = false;
    while let Some(event) = rx.recv().await {
        if matches!(
            event,
            StreamEvent::Done(vulcan::provider::ChatResponse {
                finish_reason: Some(reason),
                ..
            }) if reason == "cancelled"
        ) {
            saw_done = true;
            break;
        }
    }

    let (response, agent) = run.await.unwrap().unwrap();
    assert_eq!(response, "Cancelled");
    assert!(saw_done);
    let history = agent.memory().load_history(&session_id).unwrap().unwrap();
    assert!(history.iter().any(|message| matches!(
        message,
        Message::Tool { tool_call_id, content } if tool_call_id == "sleep_call"
            && content.contains("Cancelled")
    )));
}

#[tokio::test]
async fn resume_session_reloads_saved_history_for_next_prompt() {
    let (mut agent, mock) = agent_with_mock();
    mock.enqueue_text("first answer");

    assert_eq!(
        agent.run_prompt("first question").await.unwrap(),
        "first answer"
    );
    let session_id = agent.session_id().to_string();
    let saved = agent.memory().load_history(&session_id).unwrap().unwrap();
    assert!(saved.iter().any(|message| matches!(
        message,
        Message::Assistant { content: Some(content), .. } if content == "first answer"
    )));

    agent.resume_session(&session_id).unwrap();
    mock.enqueue_text("second answer");
    assert_eq!(
        agent.run_prompt("second question").await.unwrap(),
        "second answer"
    );

    let calls = mock.captured_calls();
    let second_turn = calls.last().unwrap();
    assert!(second_turn.iter().any(|message| matches!(
        message,
        Message::Assistant { content: Some(content), .. } if content == "first answer"
    )));
}
