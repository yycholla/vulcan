use super::*;
use crate::memory::SessionSummary;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use vulcan_frontend_api::{WidgetContent, WidgetUpdate};

#[test]
fn orchestration_state_tracks_prompt_and_tool_flow() {
    let mut app = AppState::new("test-model".into(), 128_000);
    app.note_prompt_submitted("list files in the current directory");
    assert_eq!(app.orchestration.phase, OrchestrationPhase::Thinking);
    assert!(app.orchestration.active_task.contains("list files"));

    app.note_tool_start("bash");
    assert_eq!(app.orchestration.phase, OrchestrationPhase::ToolRunning);
    assert_eq!(app.orchestration.current_tool.as_deref(), Some("bash"));

    let tree = app.tree_nodes();
    assert_eq!(tree.len(), 2);
    assert!(tree[1].label.contains("bash"));

    app.note_tool_end("bash", true);
    assert_eq!(app.orchestration.phase, OrchestrationPhase::Thinking);
    assert!(app.orchestration.current_tool.is_none());

    app.note_done();
    assert_eq!(app.orchestration.phase, OrchestrationPhase::Complete);
    assert_eq!(app.ticker_cells().last().unwrap().msg, "completed turn");
}

#[test]
fn subagent_tiles_expose_single_real_orchestrator() {
    let mut app = AppState::new("test-model".into(), 128_000);
    app.note_prompt_submitted("check auth middleware");
    let tiles = app.subagent_tiles();
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].name, "main");
    assert_eq!(tiles[0].role, "orchestrator");
    assert!(tiles[0].log[0].contains("check auth middleware"));
}

#[test]
fn hydrate_sessions_retains_lineage_and_activity_fields() {
    let mut app = AppState::new("test-model".into(), 128_000);
    let parent_id = "parent-12345678".to_string();
    let child_id = "child-87654321".to_string();
    app.hydrate_sessions(
        &[SessionSummary {
            id: child_id.clone(),
            created_at: 10,
            last_active: 20,
            message_count: 3,
            parent_session_id: Some(parent_id.clone()),
            lineage_label: Some("branched from auth cleanup".into()),
            provider_profile: None,
            preview: None,
        }],
        &child_id,
    );

    assert_eq!(app.sessions.len(), 1);
    let session = &app.sessions[0];
    assert_eq!(session.id, child_id);
    assert_eq!(
        session.parent_session_id.as_deref(),
        Some(parent_id.as_str())
    );
    assert_eq!(
        session.lineage_label.as_deref(),
        Some("branched from auth cleanup")
    );
    assert_eq!(session.status, SessionStatus::Live);
    assert!(session.is_active);
    assert_eq!(session.message_count, 3);
}

#[test]
fn segments_interleave_reasoning_tool_text_in_arrival_order() {
    // Simulates the exact YYC-71 sequence: think → tool → think → answer.
    let mut m = ChatMessage::new(ChatRole::Agent, "");
    m.append_reasoning("checking the file");
    m.push_tool_start("read_file");
    m.finish_tool("read_file", true);
    m.append_reasoning("now writing");
    m.push_tool_start("write_file");
    m.finish_tool("write_file", true);
    m.append_text("Done!");

    let kinds: Vec<&str> = m
        .segments
        .iter()
        .map(|s| match s {
            MessageSegment::Reasoning(_) => "reasoning",
            MessageSegment::Text(_) => "text",
            MessageSegment::ToolCall { .. } => "tool",
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["reasoning", "tool", "reasoning", "tool", "text"]
    );
}

#[test]
fn append_reasoning_coalesces_until_broken_by_other_segment() {
    let mut m = ChatMessage::new(ChatRole::Agent, "");
    m.append_reasoning("first ");
    m.append_reasoning("chunk");
    m.push_tool_start("bash");
    m.append_reasoning("after tool");
    // Three segments: reasoning, tool, reasoning — second reasoning is
    // its own segment because the tool call broke the run.
    assert_eq!(m.segments.len(), 3);
    match &m.segments[0] {
        MessageSegment::Reasoning(r) => assert_eq!(r, "first chunk"),
        other => panic!("expected reasoning, got {other:?}"),
    }
    match &m.segments[2] {
        MessageSegment::Reasoning(r) => assert_eq!(r, "after tool"),
        other => panic!("expected reasoning, got {other:?}"),
    }
}

#[test]
fn chat_message_render_version_bumps_on_mutation() {
    let mut m = ChatMessage::new(ChatRole::Agent, "");
    let initial = m.render_version();

    m.append_text("hello");
    assert!(m.render_version() > initial);
    let after_text = m.render_version();

    m.append_reasoning("thinking");
    assert!(m.render_version() > after_text);
    let after_reasoning = m.render_version();

    m.push_tool_start("bash");
    assert!(m.render_version() > after_reasoning);
    let after_tool_start = m.render_version();

    m.finish_tool("bash", true);
    assert!(m.render_version() > after_tool_start);
}

#[test]
fn model_status_omits_prefix_when_no_provider_label() {
    let mut app = AppState::new("deepseek/v4".into(), 128_000);
    app.prompt_tokens_last = 18_402;
    let status = app.model_status();
    assert_eq!(status, "deepseek/v4 · 18,402 / 128,000");
    assert!(!status.contains(" · deepseek/v4"));
}

#[test]
fn model_status_prefixes_active_provider_label() {
    let mut app = AppState::new("deepseek/v4".into(), 128_000);
    app.provider_label = Some("local".into());
    app.prompt_tokens_last = 18_402;
    let status = app.model_status();
    assert_eq!(status, "local · deepseek/v4 · 18,402 / 128,000");
}

#[test]
fn app_state_applies_status_widget_updates() {
    let mut app = AppState::new("deepseek/v4".into(), 128_000);

    app.apply_widget_updates(vec![
        WidgetUpdate {
            id: "job".into(),
            content: Some(WidgetContent::Spinner("working".into())),
        },
        WidgetUpdate {
            id: "progress".into(),
            content: Some(WidgetContent::progress("sync", 0.5)),
        },
    ]);

    assert_eq!(
        app.status_widgets(),
        vec![
            ("job".to_string(), WidgetContent::Spinner("working".into())),
            (
                "progress".to_string(),
                WidgetContent::Progress {
                    label: "sync".into(),
                    ratio: 0.5
                }
            ),
        ]
    );
    assert!(app.model_status().contains("working"));
    assert!(app.model_status().contains("sync 50%"));

    app.apply_widget_updates(vec![WidgetUpdate {
        id: "job".into(),
        content: None,
    }]);

    assert_eq!(app.status_widgets().len(), 1);
    assert!(!app.model_status().contains("working"));
}

#[test]
fn activity_motion_tracks_busy_queue_widgets_and_tool_segments() {
    let mut app = AppState::new("test-model".into(), 128_000);
    assert!(!app.activity_motion_active());

    app.thinking = true;
    assert!(app.activity_motion_active());
    app.thinking = false;

    app.queue.push_back("steer".into());
    assert!(app.activity_motion_active());
    app.queue.clear();

    app.apply_widget_updates(vec![WidgetUpdate {
        id: "sync".into(),
        content: Some(WidgetContent::progress("sync", 0.5)),
    }]);
    assert!(app.activity_motion_active());
    app.apply_widget_updates(vec![WidgetUpdate {
        id: "sync".into(),
        content: Some(WidgetContent::Text("done".into())),
    }]);
    assert!(!app.activity_motion_active());

    let mut message = ChatMessage::new(ChatRole::Agent, "");
    message.push_tool_start("bash");
    app.messages.push(message);
    assert!(app.activity_motion_active());
}

#[test]
fn activity_motion_advances_throbber_only_while_active() {
    let mut app = AppState::new("test-model".into(), 128_000);
    app.advance_activity_motion();
    assert_eq!(app.activity_throbber.index(), 0);

    app.thinking = true;
    app.advance_activity_motion();
    assert_eq!(app.activity_throbber.index(), 1);
}

struct TestCanvas;

impl vulcan_frontend_api::Canvas for TestCanvas {
    fn render(&self) -> vulcan_frontend_api::CanvasFrame {
        vulcan_frontend_api::CanvasFrame {
            title: "Test canvas".into(),
            lines: vec!["alive".into()],
        }
    }
}

#[test]
fn canvas_request_installs_and_exits_on_default_escape() {
    let mut app = AppState::new("test".into(), 100);
    let mut ui = vulcan_frontend_api::ExtensionUi::default();
    ui.custom(vulcan_frontend_api::CanvasFactory::new(|_handle| {
        Box::new(TestCanvas)
    }))
    .expect("canvas request");
    let request = ui.drain_canvas_requests().pop().expect("request");

    app.install_canvas_request(request);
    assert_eq!(
        app.active_canvas_frame().expect("frame").title,
        "Test canvas"
    );

    assert!(app.handle_canvas_key(vulcan_frontend_api::CanvasKey::Esc));
    assert!(!app.has_active_canvas());
}

#[test]
fn cancel_stack_pops_canvas_before_turn() {
    let mut app = AppState::new("test".into(), 100);
    app.thinking = true;
    let mut ui = vulcan_frontend_api::ExtensionUi::default();
    ui.custom(vulcan_frontend_api::CanvasFactory::new(|_handle| {
        Box::new(TestCanvas)
    }))
    .expect("canvas request");
    app.install_canvas_request(ui.drain_canvas_requests().pop().expect("request"));

    assert_eq!(
        app.cancel_stack(),
        vec![CancelScope::Turn, CancelScope::Canvas]
    );
    assert_eq!(
        app.pop_cancel_scope(),
        CancelPop::Popped(CancelScope::Canvas)
    );
    assert!(!app.has_active_canvas());
    assert_eq!(app.pop_cancel_scope(), CancelPop::CancelTurn);
}

#[test]
fn chat_message_new_starts_at_zero_render_version() {
    let m = ChatMessage::new(ChatRole::User, "hello");
    assert_eq!(m.render_version(), 0);
}

#[test]
fn refresh_prompt_mode_picks_command_when_input_starts_with_slash() {
    let mut app = AppState::new("test".into(), 100);
    app.input = "/help".into();
    app.refresh_prompt_mode();
    assert_eq!(app.prompt_mode, PromptMode::Command);
}

#[test]
fn refresh_prompt_mode_returns_to_insert_when_slash_cleared() {
    let mut app = AppState::new("test".into(), 100);
    app.input = "/help".into();
    app.refresh_prompt_mode();
    app.input.clear();
    app.refresh_prompt_mode();
    assert_eq!(app.prompt_mode, PromptMode::Insert);
}

#[test]
fn refresh_prompt_mode_uses_busy_when_thinking() {
    let mut app = AppState::new("test".into(), 100);
    app.thinking = true;
    app.refresh_prompt_mode();
    assert_eq!(app.prompt_mode, PromptMode::Busy);
    assert_eq!(app.mode_label(), "BUSY");
}

#[test]
fn refresh_prompt_mode_busy_overrides_command_prefix() {
    // While the agent is mid-turn the badge should still read BUSY
    // even if the user typed `/` in the prompt — the slash menu can
    // be shown but the mode pill reflects the agent state.
    let mut app = AppState::new("test".into(), 100);
    app.thinking = true;
    app.input = "/queue".into();
    app.refresh_prompt_mode();
    assert_eq!(app.prompt_mode, PromptMode::Busy);
}

#[test]
fn prompt_hints_default_keybinds_match_ascii_labels() {
    // Default Keybinds should produce ASCII-safe labels (Ctrl+T,
    // Ctrl+K) so prompt-row chips render in any terminal font.
    let app = AppState::new("test".into(), 100);
    let hints = app.prompt_hints();
    let pairs: Vec<(String, String)> = hints.to_vec();
    assert!(
        pairs.contains(&("Ctrl+T".into(), "tools".into())),
        "expected Ctrl+T tools in {pairs:?}"
    );
    assert!(
        pairs.contains(&("Ctrl+K".into(), "sessions".into())),
        "expected Ctrl+K sessions in {pairs:?}"
    );
}

#[test]
fn prompt_hints_reflect_overridden_keybind() {
    use super::super::keybinds::{KeyBinding, Keybinds};
    use crossterm::event::{KeyCode, KeyModifiers};
    let mut kb = Keybinds::defaults();
    kb.toggle_tools = KeyBinding {
        code: KeyCode::F(2),
        mods: KeyModifiers::NONE,
    };
    let app = AppState::new("test".into(), 100).with_keybinds(kb);
    let pairs: Vec<(String, String)> = app.prompt_hints().to_vec();
    assert!(
        pairs.contains(&("F2".into(), "tools".into())),
        "expected F2 tools in {pairs:?}"
    );
    assert!(
        !pairs.iter().any(|(k, _)| k == "Ctrl+T"),
        "stale Ctrl+T label leaked into {pairs:?}"
    );
}

#[test]
fn prompt_hints_returns_borrowed_slice_no_alloc() {
    // Two consecutive calls in the same mode must return slices to
    // the same cached storage — proves we're not allocating per call.
    let app = AppState::new("test".into(), 100);
    let first = app.prompt_hints().as_ptr();
    let second = app.prompt_hints().as_ptr();
    assert_eq!(first, second, "prompt_hints reallocated between calls");
}

#[test]
fn format_thousands_groups_at_three_digit_boundaries() {
    assert_eq!(format_thousands(0), "0");
    assert_eq!(format_thousands(42), "42");
    assert_eq!(format_thousands(999), "999");
    assert_eq!(format_thousands(1_000), "1,000");
    assert_eq!(format_thousands(18_402), "18,402");
    assert_eq!(format_thousands(1_234_567), "1,234,567");
}

#[test]
fn context_ratio_reflects_latest_prompt_size() {
    let mut app = AppState::new("test".into(), 100_000);
    assert_eq!(app.context_ratio(), 0.0);
    app.prompt_tokens_last = 50_000;
    assert!((app.context_ratio() - 0.5).abs() < 1e-6);
    app.prompt_tokens_last = 95_000;
    assert!(app.context_ratio() > 0.9);
}

#[test]
fn estimated_cost_returns_none_without_pricing() {
    let mut app = AppState::new("test".into(), 100_000);
    app.prompt_tokens_total = 1_000;
    app.completion_tokens_total = 500;
    assert!(app.estimated_cost().is_none());
}

#[test]
fn estimated_cost_multiplies_tokens_by_per_token_pricing() {
    let mut app = AppState::new("test".into(), 100_000);
    app.prompt_tokens_total = 1_000;
    app.completion_tokens_total = 500;
    app.pricing = Some(crate::provider::catalog::Pricing {
        input_per_token: 0.000_001,
        output_per_token: 0.000_002,
    });
    let cost = app.estimated_cost().unwrap();
    // 1000*0.000001 + 500*0.000002 = 0.001 + 0.001 = 0.002
    assert!((cost - 0.002).abs() < 1e-9, "got {cost}");
}

#[test]
fn lifetime_tokens_sums_prompt_and_completion_totals() {
    let mut app = AppState::new("test".into(), 100_000);
    app.prompt_tokens_total = 1_000;
    app.completion_tokens_total = 500;
    assert_eq!(app.lifetime_tokens(), 1_500);
}

#[test]
fn queue_starts_empty_and_pushes_in_fifo_order() {
    let mut app = AppState::new("test".into(), 100);
    assert!(app.queue.is_empty());
    app.queue.push_back("first".into());
    app.queue.push_back("second".into());
    assert_eq!(app.queue.pop_front().as_deref(), Some("first"));
    assert_eq!(app.queue.pop_front().as_deref(), Some("second"));
    assert!(app.queue.is_empty());
}

#[test]
fn queue_pop_back_removes_most_recent_only() {
    let mut app = AppState::new("test".into(), 100);
    app.queue.push_back("a".into());
    app.queue.push_back("b".into());
    app.queue.push_back("c".into());
    app.queue.pop_back();
    let remaining: Vec<&str> = app.queue.iter().map(String::as_str).collect();
    assert_eq!(remaining, vec!["a", "b"]);
}

#[test]
fn finish_tool_pairs_with_most_recent_in_progress_call_of_same_name() {
    // Parallel dispatch (YYC-34): two write_file calls in flight; the
    // first to finish should pair with the most recent matching start
    // that's still in-progress.
    let mut m = ChatMessage::new(ChatRole::Agent, "");
    m.push_tool_start("write_file");
    m.push_tool_start("write_file");
    m.finish_tool("write_file", true);

    let statuses: Vec<ToolStatus> = m
        .segments
        .iter()
        .filter_map(|s| match s {
            MessageSegment::ToolCall { status, .. } => Some(*status),
            _ => None,
        })
        .collect();
    // Most-recent in-progress finishes first; the older one stays open.
    assert_eq!(
        statuses,
        vec![ToolStatus::InProgress, ToolStatus::Done(true)]
    );
}

#[test]
fn push_tool_start_with_carries_params_summary() {
    let mut m = ChatMessage::new(ChatRole::Agent, "");
    m.push_tool_start_with("read_file", Some("src/foo.rs".into()));
    match &m.segments[0] {
        MessageSegment::ToolCall { params_summary, .. } => {
            assert_eq!(params_summary.as_deref(), Some("src/foo.rs"))
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

// YYC-207: with no orchestration store wired, subagent_tiles
// returns the legacy single "main" tile so demo / first-launch
// rendering stays coherent.
#[test]
fn subagent_tiles_legacy_when_no_store() {
    let app = AppState::new("test-model".into(), 128_000);
    let tiles = app.subagent_tiles();
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].name, "main");
}

// YYC-207: a populated store surfaces one tile per recent record
// alongside the orchestrator's "main" tile.
#[test]
fn subagent_tiles_include_child_records_from_store() {
    use crate::orchestration::OrchestrationStore;
    use std::sync::Arc;
    let store = Arc::new(OrchestrationStore::new());
    let r1 = store.register(None, "summarize provider parser", 8);
    store.update_status(r1.id, crate::orchestration::ChildStatus::Running);
    store.update_phase(r1.id, "thinking");
    let r2 = store.register(None, "review hooks", 4);
    store.mark_completed(r2.id, "no hot spots", 3);

    let mut app = AppState::new("test-model".into(), 128_000);
    app.orchestration_store = Some(store);
    let tiles = app.subagent_tiles();
    assert_eq!(tiles.len(), 3, "main + 2 children");
    assert_eq!(tiles[0].name, "main");
    // Newest first: r2 (completed) before r1 (running) since
    // recent() reverses insertion order.
    assert!(tiles[1].name.starts_with("child:"));
    assert_eq!(tiles[1].state, "done");
    assert_eq!(tiles[2].state, "running");
}

// YYC-207: tree_nodes appends child records as depth-1 nodes; the
// non-terminal record is marked active.
#[test]
fn tree_nodes_include_child_records_from_store() {
    use crate::orchestration::OrchestrationStore;
    use std::sync::Arc;
    let store = Arc::new(OrchestrationStore::new());
    let r = store.register(None, "review", 4);
    store.update_status(r.id, crate::orchestration::ChildStatus::Running);
    let mut app = AppState::new("test-model".into(), 128_000);
    app.orchestration_store = Some(store);
    let nodes = app.tree_nodes();
    let child = nodes
        .iter()
        .find(|n| n.label.contains("child:"))
        .expect("child node");
    assert_eq!(child.depth, 1);
    assert!(child.active);
}

// YYC-207: delegated_worker_count counts only non-terminal records.
#[test]
fn delegated_worker_count_filters_terminal_records() {
    use crate::orchestration::OrchestrationStore;
    use std::sync::Arc;
    let store = Arc::new(OrchestrationStore::new());
    let r1 = store.register(None, "live", 4);
    store.update_status(r1.id, crate::orchestration::ChildStatus::Running);
    let r2 = store.register(None, "done", 4);
    store.mark_completed(r2.id, "ok", 1);
    let mut app = AppState::new("test-model".into(), 128_000);
    app.orchestration_store = Some(store);
    assert_eq!(app.delegated_worker_count(), 1);
}

#[test]
fn prompt_editor_uses_shift_enter_for_multiline_insert_mode() {
    let mut app = AppState::new("test-model".into(), 128_000);
    app.prompt_insert_str("first");
    app.prompt_handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.input, "first");

    app.prompt_handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    app.prompt_insert_str("second");

    assert_eq!(app.input, "first\nsecond");
    assert_eq!(app.prompt_editor.mode(), PromptEditMode::Insert);
}

#[test]
fn prompt_editor_esc_enters_vim_normal_mode_and_i_returns_to_insert() {
    let mut app = AppState::new("test-model".into(), 128_000);
    app.prompt_insert_str("hello");

    app.prompt_handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(app.prompt_editor.mode(), PromptEditMode::Normal);
    assert_eq!(app.mode_label(), "NORMAL");

    app.prompt_handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    assert_eq!(app.prompt_editor.mode(), PromptEditMode::Insert);
    assert_eq!(app.mode_label(), "INSERT");
}

#[test]
fn finish_tool_with_stamps_preview_and_timing() {
    let mut m = ChatMessage::new(ChatRole::Agent, "");
    m.push_tool_start("read_file");
    m.finish_tool_with(
        "read_file",
        true,
        Some("hello\nworld".into()),
        Some("2 lines".into()),
        0,
        Some(345),
    );
    match &m.segments[0] {
        MessageSegment::ToolCall {
            output_preview,
            elapsed_ms,
            status,
            ..
        } => {
            assert!(matches!(status, ToolStatus::Done(true)));
            assert_eq!(output_preview.as_deref(), Some("hello\nworld"));
            assert_eq!(*elapsed_ms, Some(345));
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}
