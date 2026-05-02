//! TUI entry point.
//!
//! ## YYC-266 — module size investigation
//!
//! The audit (`codebase-analysis.md` A2) flagged `tui/mod.rs` as
//! oversized at "101 KB"; the actual figure is ~1675 lines, and
//! the file holds `run_tui` (the event loop) plus a single small
//! helper. Heavy decomposition has already happened — the TUI
//! is split across 15+ submodules:
//!
//! - State + diffing: `state/`, `chat_message`, `chat_render`.
//! - Rendering: `rendering`, `widgets`, `views`, `markdown`,
//!   `theme`, `miller_columns`, `model_picker`, `picker_state`.
//! - Input: `events`, `keybinds`, `keymap`.
//! - Init / orchestration: `init`, `orchestration`.
//!
//! What's left in `mod.rs` is the orchestrator — the event loop,
//! the streaming-event pump, slash-command dispatch, and pause /
//! resume wiring. Every line couples to multiple submodules; a
//! further split would mean either passing huge tuples of state
//! across module boundaries or pulling submodules back into a
//! shared file. Neither is a clear win.
//!
//! Decision: leave the orchestrator in `mod.rs`. New code that
//! adds a *new* coherent surface (e.g. a future plugin host)
//! lives in its own submodule from day one.

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    layout::Rect,
    prelude::Position,
};
use tokio::sync::{Mutex, mpsc};

use crate::agent::Agent;
use crate::config::Config;
use crate::hooks::HookRegistry;
use crate::hooks::audit::AuditHook;
use crate::pause::{self, AgentResume, PauseKind};
use crate::provider::StreamEvent;

pub mod chat_message;
pub mod chat_render;
mod events;
pub mod frontend;
mod init;
pub mod keybinds;
mod keymap;
mod layouts;
pub mod markdown;
pub mod miller_columns;
pub mod model_picker;
pub mod orchestration;
pub mod picker_state;
pub mod prompt;
mod rendering;
pub mod state;
mod surface;
pub mod theme;
pub mod views;
pub mod widgets;

use state::{AppState, CancelPop, ChatMessage, ChatRole, PromptEnterIntent, PromptEscapeIntent};
use theme::{Theme, body};
use views::{View, render_view};
use vulcan_frontend_api::{CanvasKey, FrontendCommandAction};
use widgets::ProviderPickerWidget;

/// What session, if any, the TUI should load on startup.
#[derive(Debug, Clone)]
pub enum ResumeTarget {
    /// Start fresh — new session, empty history.
    None,
    /// Resume the most recently active session.
    Last,
    /// Resume a specific session by ID.
    Specific(String),
    /// Open the TUI with a session-picker overlay so the user can
    /// choose which session to resume. Falls back to fresh if
    /// dismissed without a selection.
    Pick,
}

enum KeyEv {
    Press(Event),
    Error(String),
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn apply_frontend_command_action(app: &mut AppState, action: FrontendCommandAction) {
    match action {
        FrontendCommandAction::Noop => {}
        FrontendCommandAction::SystemMessage(content) => {
            app.messages
                .push(ChatMessage::new(ChatRole::System, content));
        }
        FrontendCommandAction::OpenView { title, body, .. } => {
            let content = if body.is_empty() {
                title
            } else {
                format!("{title}\n{}", body.join("\n"))
            };
            app.messages
                .push(ChatMessage::new(ChatRole::System, content));
        }
    }
}

fn apply_frontend_dispatch(app: &mut AppState, dispatch: frontend::FrontendCommandDispatch) {
    apply_frontend_command_action(app, dispatch.action);
    for update in dispatch.widget_updates {
        app.apply_widget_updates(vec![update]);
    }
    for request in dispatch.tick_requests {
        app.install_tick_request(request);
    }
    for request in dispatch.canvas_requests {
        app.install_canvas_request(request);
    }
}

fn canvas_key_from_event(key: ratatui::crossterm::event::KeyEvent) -> CanvasKey {
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return CanvasKey::CtrlC;
    }
    match key.code {
        KeyCode::Up => CanvasKey::Up,
        KeyCode::Down => CanvasKey::Down,
        KeyCode::Left => CanvasKey::Left,
        KeyCode::Right => CanvasKey::Right,
        KeyCode::Esc => CanvasKey::Esc,
        KeyCode::Enter => CanvasKey::Enter,
        KeyCode::Backspace => CanvasKey::Backspace,
        KeyCode::Char(c) => CanvasKey::Char(c),
        other => CanvasKey::Other(format!("{other:?}")),
    }
}

pub async fn run_tui(
    config: &Config,
    resume: ResumeTarget,
    tool_profile: Option<String>,
) -> Result<()> {
    let mut terminal = init::init_terminal()?;

    // keyboard
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<KeyEv>();
    let tx_keys = key_tx.clone();
    std::thread::spawn(move || {
        loop {
            match event::read() {
                Ok(ev) => {
                    if tx_keys.send(KeyEv::Press(ev)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx_keys.send(KeyEv::Error(e.to_string()));
                    break;
                }
            }
        }
    });

    // streaming. YYC-147: capacity sourced from the active provider
    // config so users can tune for slow renderers (raise) or
    // memory-constrained hosts (lower).
    let (stream_tx, mut stream_rx) =
        mpsc::channel::<StreamEvent>(config.provider.effective_stream_channel_capacity());
    let frontend = frontend::TuiFrontend::collect();

    // ── Hook registry: audit-log + (room for safety-gate, etc.). Built-in
    // hooks (skills) are registered by AgentBuilder.
    let hook_reg = HookRegistry::new();
    let (audit_hook, audit_buf) = AuditHook::new(200);
    hook_reg.register(audit_hook);

    // ── AgentPause channel: when SafetyHook (or future hooks) needs user
    // input mid-loop, it sends an AgentPause; the main TUI loop renders an
    // overlay and routes the response back via the pause's oneshot reply.
    let (pause_tx, mut pause_rx) = pause::channel(8);
    let pause_tx_for_agent = pause_tx.clone();
    let (frontend_event_tx, mut frontend_event_rx) = tokio::sync::broadcast::channel(32);

    // ── Long-lived agent: one per TUI session, shared across prompts so
    // hook handlers' state (audit log, rate limits, etc.) survives turns.
    let agent = Arc::new(Mutex::new(
        Agent::builder(config)
            .with_hooks(hook_reg)
            .with_pause_channel(pause_tx_for_agent)
            .with_tool_profile(tool_profile)
            .with_frontend_context(
                frontend.extension_frontend_capabilities(),
                frontend.frontend_extensions(),
                crate::extensions::api::FrontendEventSink::new(frontend_event_tx),
            )
            .build()
            .await?,
    ));

    // ── Apply resume target if any. Errors here are non-fatal — we report
    // and start fresh.
    let resume_note = {
        let mut a = agent.lock().await;
        let outcome = match &resume {
            ResumeTarget::None => None,
            ResumeTarget::Pick => None, // session picker shown in UI later
            ResumeTarget::Last => match a.continue_last_session() {
                Ok(()) => Some(Ok(format!(
                    "Resumed last session ({})",
                    short_id(a.session_id())
                ))),
                Err(e) => Some(Err(format!("Could not resume last session: {e}"))),
            },
            ResumeTarget::Specific(id) => match a.resume_session(id) {
                Ok(()) => Some(Ok(format!("Resumed session {}", short_id(id)))),
                Err(e) => Some(Err(format!("Could not resume session: {e}"))),
            },
        };
        match outcome {
            Some(Ok(note)) => {
                if let Err(e) = a.restore_persisted_provider(config).await {
                    tracing::warn!("provider restore failed during resume: {e}");
                }
                Some(note)
            }
            Some(Err(err)) => Some(err),
            None => None,
        }
    };

    {
        let a = agent.lock().await;
        a.start_session().await;
    }

    let mut app = AppState::new(
        config.provider.model.clone(),
        config.provider.max_context as u32,
    )
    .with_theme(Theme::from_name(&config.tui.theme))
    .with_keybinds(keybinds::Keybinds::from_config(&config.keybinds));
    app.audit_log = Some(audit_buf);
    app.frontend = frontend;
    // YYC-66: clone the agent's diff sink so the TUI can render real edits.
    // YYC-67: pull catalog pricing for the cost estimate.
    // YYC-95: if resume restored a provider profile the active model/context
    // window changed under us — sync the app surface from the agent.
    // YYC-96: surface the active profile name in the prompt-row status.
    {
        let a = agent.lock().await;
        app.diff_sink = Some(a.diff_sink().clone());
        app.pricing = a.pricing().cloned();
        app.model_label = a.active_model().to_string();
        app.token_max = a.max_context() as u32;
        app.provider_label = a.active_profile().map(str::to_string);
        // YYC-207: share the agent's orchestration store so subagent
        // tiles + tree nodes render real child runs as they happen.
        app.orchestration_store = Some(a.orchestration());
    }
    events::refresh_sessions(&agent, &mut app).await;

    // YYC-86: if the user invoked --resume, activate the session picker.
    app.show_session_picker = matches!(resume, ResumeTarget::Pick);

    if let Some(note) = resume_note {
        app.messages.push(ChatMessage {
            role: ChatRole::System,
            content: note,
            reasoning: String::new(),
            segments: Vec::new(),
            render_version: 0,
        });

        // Hydrate prior turns into the chat panel so resumed sessions show their
        // history, not a blank screen. Tool turns are skipped (audit log surfaces
        // tool activity separately).
        let history = {
            let a = agent.lock().await;
            a.memory().load_history(a.session_id()).ok().flatten()
        };
        if let Some(msgs) = history {
            for msg in msgs {
                use crate::provider::Message;
                match msg {
                    Message::User { content } => {
                        app.messages.push(ChatMessage::new(ChatRole::User, content));
                    }
                    Message::System { content } => {
                        app.messages
                            .push(ChatMessage::new(ChatRole::System, content));
                    }
                    Message::Assistant {
                        content,
                        reasoning_content,
                        ..
                    } => {
                        app.messages.push(ChatMessage {
                            role: ChatRole::Agent,
                            content: content.unwrap_or_default(),
                            reasoning: reasoning_content.unwrap_or_default(),
                            segments: Vec::new(),
                            render_version: 0,
                        });
                    }
                    Message::Tool { .. } => {} // skip — audit log shows tool activity
                }
            }
        }
    }

    let mut exit = false;
    let mut pending_quit = false;
    let mut last_draw: Instant;

    while !exit {
        let palette = if app.input.starts_with('/') && app.input.len() > 1 {
            Some(keymap::filter_commands(&app.input[1..]))
        } else if app.input == "/" {
            Some(keymap::SLASH_COMMANDS.iter().collect())
        } else {
            None
        };

        // YYC-58: derive prompt mode from current state once per tick.
        app.refresh_prompt_mode();
        app.pump_frontend_ticks();

        // YYC-69: keep the chat viewport pinned to the latest content while
        // the user hasn't scrolled away. `chat_max_scroll` is published by
        // the renderer on the previous frame, so this lags one tick after a
        // new event — invisible in practice.
        if app.at_bottom {
            app.scroll = app.chat_max_scroll.get();
        }

        terminal.draw(|f| {
            let area = f.area();
            let (main_area, palette_area) = if let Some(ref pal) = palette {
                let h = (pal.len() as u16 + 2).min(area.height / 2);
                (
                    Rect {
                        x: area.x,
                        y: area.y,
                        width: area.width,
                        height: area.height - h,
                    },
                    Some(Rect {
                        x: area.x,
                        y: area.y + area.height - h,
                        width: area.width,
                        height: h,
                    }),
                )
            } else {
                (area, None)
            };

            render_view(f, main_area, &app);
            let (cx, cy) = app.cursor();
            if cx >= main_area.x
                && cx < main_area.x + main_area.width
                && cy >= main_area.y
                && cy < main_area.y + main_area.height
            {
                f.set_cursor_position(Position::new(cx, cy));
            }

            if let (Some(pal), Some(area)) = (palette.as_ref(), palette_area) {
                rendering::draw_palette(f, area, pal, app.slash_menu_selection, &app.theme);
            }

            // YYC-86: session picker overlay (--resume flag).
            if app.show_session_picker {
                rendering::draw_session_picker(f, area, &app);
            }
            // YYC-97: model / provider picker overlays.
            if app.show_model_picker {
                rendering::draw_model_picker(f, area, &app);
            }
            if let Some(surface::SurfaceFrame::ProviderPicker { items, selection }) =
                app.active_surface_frame()
            {
                f.render_widget(
                    ProviderPickerWidget {
                        theme: &app.theme,
                        items: &items,
                        selection,
                    },
                    area,
                );
            }
            // YYC-75: diff scrubber overlay.
            if app.show_diff_scrubber {
                rendering::draw_diff_scrubber(f, area, &app);
            }
        })?;
        last_draw = Instant::now();

        // ── Diff scrubber overlay (YYC-75): intercept input until resolved.
        if app.show_diff_scrubber {
            tokio::select! {
                ev = key_rx.recv() => {
                    match ev {
                        Some(KeyEv::Press(event)) => {
                            if let Event::Key(key) = event
                                && key.kind == KeyEventKind::Press {
                                    let total = app.scrubber_hunks.len();
                                    match key.code {
                                        KeyCode::Up | KeyCode::Char('k') => {
                                            app.scrubber_selection = app.scrubber_selection.saturating_sub(1);
                                        }
                                        KeyCode::Down | KeyCode::Char('j') => {
                                            app.scrubber_selection = app.scrubber_selection.saturating_add(1).min(total.saturating_sub(1));
                                        }
                                        KeyCode::Char('y') => {
                                            if let Some(slot) = app.scrubber_accepted.get_mut(app.scrubber_selection) {
                                                *slot = !*slot;
                                            }
                                        }
                                        KeyCode::Char('Y') => {
                                            for slot in &mut app.scrubber_accepted {
                                                *slot = true;
                                            }
                                        }
                                        KeyCode::Char('n') => {
                                            if let Some(slot) = app.scrubber_accepted.get_mut(app.scrubber_selection) {
                                                *slot = false;
                                            }
                                        }
                                        KeyCode::Char('N') => {
                                            for slot in &mut app.scrubber_accepted {
                                                *slot = false;
                                            }
                                        }
                                        KeyCode::Enter => {
                                            let indices: Vec<usize> = app
                                                .scrubber_accepted
                                                .iter()
                                                .enumerate()
                                                .filter_map(|(i, ok)| if *ok { Some(i) } else { None })
                                                .collect();
                                            if let Some(p) = app.scrubber_pause.take() {
                                                let _ = p.reply.send(AgentResume::AcceptHunks(indices.clone()));
                                            }
                                            let label = if indices.is_empty() {
                                                "no hunks accepted — file unchanged"
                                            } else if indices.len() == total {
                                                "all hunks accepted"
                                            } else {
                                                "subset of hunks accepted"
                                            };
                                            app.messages.push(ChatMessage {
                                                role: ChatRole::System,
                                                content: format!(
                                                    "▶  edit_file resumed — {} ({}/{})",
                                                    label, indices.len(), total
                                                ),
                                                ..Default::default()
                                            });
                                            app.show_diff_scrubber = false;
                                            app.scrubber_hunks.clear();
                                            app.scrubber_accepted.clear();
                                            app.scrubber_path.clear();
                                            app.scrubber_selection = 0;
                                        }
                                        KeyCode::Esc => {
                                            if let Some(p) = app.scrubber_pause.take() {
                                                let _ = p.reply.send(AgentResume::AcceptHunks(Vec::new()));
                                            }
                                            app.messages.push(ChatMessage {
                                                role: ChatRole::System,
                                                content: "▶  edit_file cancelled — file unchanged".into(),
                                                ..Default::default()
                                            });
                                            app.show_diff_scrubber = false;
                                            app.scrubber_hunks.clear();
                                            app.scrubber_accepted.clear();
                                            app.scrubber_path.clear();
                                            app.scrubber_selection = 0;
                                        }
                                        _ => {}
                                    }
                                }
                        }
                        Some(KeyEv::Error(e)) => {
                            tracing::error!("Terminal input error (diff scrubber): {e}");
                            if let Some(p) = app.scrubber_pause.take() {
                                let _ = p.reply.send(AgentResume::AcceptHunks(Vec::new()));
                            }
                            app.show_diff_scrubber = false;
                        }
                        None => {
                            if let Some(p) = app.scrubber_pause.take() {
                                let _ = p.reply.send(AgentResume::AcceptHunks(Vec::new()));
                            }
                            app.show_diff_scrubber = false;
                        }
                    }
                }
            }
            continue;
        }

        // ── Hierarchical model picker (YYC-101): miller columns,
        // hjkl drill-down. Intercepts input until dismissed.
        if app.show_model_picker {
            tokio::select! {
                ev = key_rx.recv() => {
                    match ev {
                        Some(KeyEv::Press(event)) => {
                            if let Event::Key(key) = event
                                && key.kind == KeyEventKind::Press {
                                    let mut commit: Option<(Option<String>, String)> = None;
                                    let mut close = false;
                                    let mut state = miller_columns::MillerState {
                                        path: app.model_picker_path.clone(),
                                        focus: app.model_picker_focus,
                                    };
                                    let source = model_picker::UnifiedPickerSource {
                                        provider_labels: &app.picker_provider_labels,
                                        provider_keys: &app.picker_provider_keys,
                                        items_by_key: &app.picker_items_by_key,
                                        trees_by_key: &app.picker_trees_by_key,
                                    };
                                    let resolve_leaf = |path: &[usize]| -> Option<(Option<String>, String)> {
                                        source.leaf_at(path).map(|(k, id)| {
                                            let key = if k == "default" { None } else { Some(k) };
                                            (key, id)
                                        })
                                    };
                                    match key.code {
                                        KeyCode::Up | KeyCode::Char('k') => {
                                            miller_columns::move_cursor(&mut state, &source, -1);
                                        }
                                        KeyCode::Down | KeyCode::Char('j') => {
                                            miller_columns::move_cursor(&mut state, &source, 1);
                                        }
                                        KeyCode::Left | KeyCode::Char('h') => {
                                            if !miller_columns::ascend(&mut state) {
                                                close = true;
                                            }
                                        }
                                        KeyCode::Right | KeyCode::Char('l') => {
                                            if !miller_columns::drill(&mut state, &source) {
                                                commit = resolve_leaf(&state.path);
                                            }
                                        }
                                        KeyCode::Char('L') => {
                                            miller_columns::drill(&mut state, &source);
                                            commit = resolve_leaf(&state.path);
                                        }
                                        KeyCode::Char('H') => {
                                            if !miller_columns::ascend(&mut state) {
                                                close = true;
                                            }
                                        }
                                        KeyCode::Enter => {
                                            commit = resolve_leaf(&state.path);
                                        }
                                        KeyCode::Esc | KeyCode::Char('q') => close = true,
                                        _ => {}
                                    }
                                    app.model_picker_path = state.path;
                                    app.model_picker_focus = state.focus;
                                    if let Some((profile, id)) = commit {
                                        let result = {
                                            let mut a = agent.lock().await;
                                            // Switch provider first when the
                                            // picked profile differs from the
                                            // active one.
                                            let active = a.active_profile().map(str::to_string);
                                            if active != profile {
                                                a.switch_provider_model(
                                                    profile.as_deref(),
                                                    config,
                                                    &id,
                                                )
                                                .await
                                            } else {
                                                a.switch_model(&id).await
                                            }
                                        };
                                        match result {
                                            Ok(selection) => {
                                                app.model_label =
                                                    selection.model.id.clone();
                                                app.token_max =
                                                    selection.max_context as u32;
                                                app.pricing = selection.pricing;
                                                app.provider_label = profile.clone();
                                                let label = profile
                                                    .as_deref()
                                                    .unwrap_or("default");
                                                app.messages.push(ChatMessage {
                                                    role: ChatRole::System,
                                                    content: format!(
                                                        "Switched to {label} · {} · context {}",
                                                        app.model_label,
                                                        crate::tui::state::format_thousands(
                                                            app.token_max
                                                        ),
                                                    ),
                                                    ..Default::default()
                                                });
                                            }
                                            Err(e) => {
                                                app.messages.push(ChatMessage {
                                                    role: ChatRole::System,
                                                    content: format!("Model switch failed: {e}"),
                                                    ..Default::default()
                                                });
                                            }
                                        }
                                        close = true;
                                    }
                                    if close {
                                        app.show_model_picker = false;
                                        app.model_picker_path.clear();
                                        app.model_picker_focus = 0;
                                    }
                                }
                        }
                        Some(KeyEv::Error(e)) => {
                            tracing::error!("Terminal input error (model picker): {e}");
                            app.show_model_picker = false;
                        }
                        None => app.show_model_picker = false,
                    }
                }
            }
            continue;
        }

        // ── Provider picker overlay (YYC-97): intercept input until dismissed.
        if app.has_provider_picker() {
            tokio::select! {
                ev = key_rx.recv() => {
                    match ev {
                        Some(KeyEv::Press(event)) => {
                            if let Event::Key(key) = event
                                && key.kind == KeyEventKind::Press {
                                    match key.code {
                                        KeyCode::Up | KeyCode::Char('k') => {
                                            app.provider_picker_up();
                                        }
                                        KeyCode::Down | KeyCode::Char('j') => {
                                            app.provider_picker_down();
                                        }
                                        KeyCode::Enter => {
                                            if let Some(picked) = app.selected_provider() {
                                                let target: Option<&str> = picked.name.as_deref();
                                                let result = {
                                                    let mut a = agent.lock().await;
                                                    a.switch_provider(target, config).await
                                                };
                                                match result {
                                                    Ok(selection) => {
                                                        app.model_label = selection.model.id.clone();
                                                        app.token_max = selection.max_context as u32;
                                                        app.pricing = selection.pricing;
                                                        app.provider_label = target.map(str::to_string);
                                                        let label = target.unwrap_or("default");
                                                        app.messages.push(ChatMessage {
                                                            role: ChatRole::System,
                                                            content: format!(
                                                                "Provider switched to {label} · {} · context {}",
                                                                app.model_label,
                                                                crate::tui::state::format_thousands(app.token_max),
                                                            ),
                                                            ..Default::default()
                                                        });
                                                    }
                                                    Err(e) => {
                                                        app.messages.push(ChatMessage {
                                                            role: ChatRole::System,
                                                            content: format!("Provider switch failed: {e}"),
                                                            ..Default::default()
                                                        });
                                                    }
                                                }
                                            }
                                            app.close_provider_picker();
                                        }
                                        KeyCode::Esc => {
                                            app.close_provider_picker();
                                        }
                                        _ => {}
                                    }
                                }
                        }
                        Some(KeyEv::Error(e)) => {
                            tracing::error!("Terminal input error (provider picker): {e}");
                            app.close_provider_picker();
                        }
                        None => {
                            app.close_provider_picker();
                        }
                    }
                }
            }
            continue;
        }

        // ── Session picker mode: intercept all input until dismissed.
        if app.show_session_picker {
            tokio::select! {
                ev = key_rx.recv() => {
                    match ev {
                        Some(KeyEv::Press(event)) => {
                            if let Event::Key(key) = event
                                && key.kind == KeyEventKind::Press {
                                    match key.code {
                                        KeyCode::Up | KeyCode::Char('k') => {
                                            app.session_picker_selection = app.session_picker_selection.saturating_sub(1);
                                        }
                                        KeyCode::Down | KeyCode::Char('j') => {
                                            let max = app.sessions.len().saturating_sub(1);
                                            app.session_picker_selection = app.session_picker_selection.saturating_add(1).min(max);
                                        }
                                        KeyCode::Enter => {
                                            let idx = app.session_picker_selection.min(app.sessions.len().saturating_sub(1));
                                            let picked = app.sessions[idx].id.clone();
                                            let current = app.active_session_id.clone().unwrap_or_default();

                                            if picked == current {
                                                // Already on this session — just dismiss.
                                                app.show_session_picker = false;
                                            } else {
                                                // Resume the selected session, then hydrate.
                                                let (note, should_hydrate) = {
                                                    let mut a = agent.lock().await;
                                                    match a.resume_session(&picked) {
                                                        Ok(()) => {
                                                            if let Err(e) = a.restore_persisted_provider(config).await {
                                                                tracing::warn!("provider restore failed during picker resume: {e}");
                                                            }
                                                            (Some(format!("Resumed session {}", short_id(&picked))), true)
                                                        }
                                                        Err(e) => (Some(format!("Could not resume session: {e}")), false),
                                                    }
                                                };
                                                app.show_session_picker = false;
                                                if should_hydrate {
                                                    let a = agent.lock().await;
                                                    app.model_label = a.active_model().to_string();
                                                    app.token_max = a.max_context() as u32;
                                                    app.pricing = a.pricing().cloned();
                                                    app.provider_label = a.active_profile().map(str::to_string);
                                                }
                                                if let Some(n) = note {
                                                    app.messages.push(ChatMessage {
                                                        role: ChatRole::System,
                                                        content: n,
                                                        ..Default::default()
                                                    });
                                                }
                                                if should_hydrate {
                                                    let history = {
                                                        let a = agent.lock().await;
                                                        a.memory().load_history(&picked).ok().flatten()
                                                    };
                                                    if let Some(msgs) = history {
                                                        for msg in msgs {
                                                            use crate::provider::Message;
                                                            match msg {
                                                                Message::User { content } => {
                                                                    app.messages.push(ChatMessage::new(ChatRole::User, content));
                                                                }
                                                                Message::System { content } => {
                                                                    app.messages.push(ChatMessage::new(ChatRole::System, content));
                                                                }
                                                                Message::Assistant { content, reasoning_content, .. } => {
                                                                    app.messages.push(ChatMessage {
                                                                        role: ChatRole::Agent,
                                                                        content: content.unwrap_or_default(),
                                                                        reasoning: reasoning_content.unwrap_or_default(),
                                                                        segments: Vec::new(),
                                                                        render_version: 0,
                                                                    });
                                                                }
                                                                Message::Tool { .. } => {}
                                                            }
                                                        }
                                                    }
                                                }
                                                events::refresh_sessions(&agent, &mut app).await;
                                            }
                                        }
                                        KeyCode::Esc => {
                                            app.show_session_picker = false;
                                            app.messages.push(ChatMessage {
                                                role: ChatRole::System,
                                                content: "Starting a new session — use /search to find past conversations.".into(),
                                                ..Default::default()
                                            });
                                        }
                                        _ => {}
                                    }
                                }
                        }
                        Some(KeyEv::Error(e)) => {
                            tracing::error!("Terminal input error (picker): {e}");
                            app.show_session_picker = false;
                        }
                        None => app.show_session_picker = false,
                    }
                }
                pause = pause_rx.recv() => {
                    // If a pause arrives while the picker is open, dismiss the
                    // picker and let the normal loop handle it.
                    if let Some(p) = pause {
                        app.show_session_picker = false;
                        // Re-route to normal pause handling by pushing it back.
                        // The pause channel is multi-consumer safe; redeliver.
                        // In practice this won't happen because no agent turn
                        // is running at startup, but be defensive.
                        if let Err(e) = pause_tx.send(p).await {
                            tracing::warn!("failed to re-route pause: {e}");
                        }
                    }
                }
            }
            continue;
        }

        tokio::select! {
            pause = pause_rx.recv() => {
                if let Some(p) = pause {
                    // YYC-59: pause now carries inline pill options, so the
                    // bracket-list hint is redundant when options is present.
                    // YYC-75: DiffScrub takes the picker route, not the
                    // pill prompt route. Capture state and let the
                    // dedicated overlay drive the response.
                    if let PauseKind::DiffScrub { path, hunks } = &p.kind {
                        app.scrubber_path = path.clone();
                        app.scrubber_hunks = hunks.clone();
                        app.scrubber_accepted = vec![true; hunks.len()];
                        app.scrubber_selection = 0;
                        app.show_diff_scrubber = true;
                        app.scrubber_pause = Some(p);
                        continue;
                    }
                    let summary = match (&p.kind, p.options.is_empty()) {
                        (PauseKind::SafetyApproval { command, reason, .. }, false) => {
                            format!("Safety: {reason}\n  $ {command}")
                        }
                        (PauseKind::ToolArgConfirm { tool, summary, .. }, false) => {
                            format!("Confirm tool '{tool}': {summary}")
                        }
                        (PauseKind::SkillSave { suggested_name, .. }, false) => {
                            format!("Save this as a skill named '{suggested_name}'?")
                        }
                        // YYC-81: ask_user always supplies its own pills,
                        // so the legacy hint case is unreachable but kept
                        // for exhaustiveness.
                        (PauseKind::UserChoice { question }, _) => question.clone(),
                        // No options → legacy bracket-list hint stays in.
                        (PauseKind::SafetyApproval { command, reason, .. }, true) => {
                            format!("Safety: {reason}\n  $ {command}\n  [a]llow once, [r]emember & allow, [d]eny")
                        }
                        (PauseKind::ToolArgConfirm { tool, summary, .. }, true) => {
                            format!("Confirm tool '{tool}': {summary}\n  [a]llow once, [r]emember & allow, [d]eny")
                        }
                        (PauseKind::SkillSave { suggested_name, .. }, true) => {
                            format!("Save this as a skill named '{suggested_name}'?\n  [a]llow once, [d]eny")
                        }
                        // DiffScrub handled above; arm kept for exhaustiveness.
                        (PauseKind::DiffScrub { .. }, _) => unreachable!(),
                        // GH issue #557: extension wants to rewrite raw
                        // user input. Show before/after to the user.
                        (
                            PauseKind::InputRewriteApproval {
                                extension_id,
                                before,
                                after,
                            },
                            false,
                        ) => format!(
                            "Extension '{extension_id}' proposes input rewrite:\n  before: {before}\n  after:  {after}"
                        ),
                        (
                            PauseKind::InputRewriteApproval {
                                extension_id,
                                before,
                                after,
                            },
                            true,
                        ) => format!(
                            "Extension '{extension_id}' proposes input rewrite:\n  before: {before}\n  after:  {after}\n  [a]llow once, [d]eny"
                        ),
                    };
                    app.messages.push(ChatMessage {
                        role: ChatRole::System,
                        content: format!("⏸  Agent paused — {summary}"),
                        reasoning: String::new(),
                            segments: Vec::new(),
                        render_version: 0,
                    });
                    app.note_pause(&summary);
                    app.pending_pause = Some(p);
                }
                continue;
            }
            ev = key_rx.recv() => {
                match ev {
                    Some(KeyEv::Press(event)) => {
                        // YYC-124: bracketed-paste payload — terminals that
                        // support CSI 200~/201~ deliver the whole pasted
                        // buffer as one Event::Paste(String) instead of N
                        // separate KeyCode::Char events. Append the chunk
                        // to the input buffer in one shot; embedded
                        // newlines stay literal so multiline pastes don't
                        // submit a prompt per line. Skipped while a pause
                        // overlay is active so the paste can't smuggle
                        // text into the resume keystroke handler.
                        if let Event::Paste(text) = &event {
                            if app.pending_pause.is_none() {
                                app.prompt_insert_str(text);
                            }
                            continue;
                        }
                        // YYC-123: mouse wheel drives the chat viewport
                        // directly. Three lines per notch matches a
                        // typical terminal scroll feel; PageUp/PageDown
                        // remain available for bigger jumps.
                        if let Event::Mouse(m) = &event {
                            use ratatui::crossterm::event::MouseEventKind;
                            const SCROLL_LINES: u16 = 3;
                            match m.kind {
                                MouseEventKind::ScrollUp => {
                                    app.scroll = app.scroll.saturating_sub(SCROLL_LINES);
                                    app.at_bottom = false;
                                }
                                MouseEventKind::ScrollDown => {
                                    let max = app.chat_max_scroll.get();
                                    app.scroll =
                                        app.scroll.saturating_add(SCROLL_LINES).min(max);
                                    app.at_bottom = app.scroll >= max;
                                }
                                _ => {}
                            }
                            continue;
                        }
                        if let Event::Key(key) = event
                            && key.kind == KeyEventKind::Press {
                                if app.has_active_canvas() {
                                    app.handle_canvas_key(canvas_key_from_event(key));
                                    pending_quit = false;
                                    continue;
                                }

                                // ── If a pause is active, intercept the keys that
                                // dispatch a response. Anything else falls through
                                // to normal handling so the user can still scroll, etc.
                                if let Some(p) = app.pending_pause.as_ref() {
                                    // YYC-59: if the pause carries inline options, the
                                    // user's keystroke is matched against options[i].key
                                    // (case-insensitive) and the option's `resume` is
                                    // sent back. Esc is always Deny. Falls back to the
                                    // legacy a/r/d modal when options is empty.
                                    let resume = if !p.options.is_empty() {
                                        match key.code {
                                            KeyCode::Esc => Some(AgentResume::Deny),
                                            KeyCode::Char(c) => p
                                                .options
                                                .iter()
                                                .find(|o| {
                                                    o.key.eq_ignore_ascii_case(&c)
                                                })
                                                .map(|o| o.resume.clone()),
                                            _ => None,
                                        }
                                    } else {
                                        match key.code {
                                            KeyCode::Char('a') | KeyCode::Char('A') => Some(AgentResume::Allow),
                                            KeyCode::Char('r') | KeyCode::Char('R') => Some(AgentResume::AllowAndRemember),
                                            KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Esc => Some(AgentResume::Deny),
                                            _ => None,
                                        }
                                    };
                                    if let Some(r) = resume {
                                        if let Some(p) = app.pending_pause.take() {
                                            let label = match &r {
                                                AgentResume::Allow => "allowed (once)",
                                                AgentResume::AllowAndRemember => "allowed (remembered)",
                                                AgentResume::Deny => "denied",
                                                AgentResume::DenyWithReason(_) => "denied",
                                                AgentResume::Custom(_) => "responded",
                                                AgentResume::AcceptHunks(_) => "applied",
                                            };
                                            let _ = p.reply.send(r);
                                            app.messages.push(ChatMessage {
                                                role: ChatRole::System,
                                                content: format!("▶  Resumed — {label}"),
                                                reasoning: String::new(),
                            segments: Vec::new(),
                                                render_version: 0,
                                            });
                                            app.note_resume(label);
                                        }
                                        continue;
                                    }
                                }

                                // ── configurable bindings (YYC-90).
                                // Run before the legacy Ctrl-modifier match
                                // so user overrides win. The session-picker
                                // bind only fires when the slash menu isn't
                                // active, so Ctrl+K nav still works while
                                // typing a `/command`. Cancel and queue_drop
                                // stay in the hardcoded match below because
                                // they carry compound behavior (pending_quit,
                                // Shift-to-clear) that doesn't reduce to a
                                // simple binding.
                                if app.keybinds.toggle_tools.matches(&key) {
                                    app.view = View::TradingFloor;
                                    continue;
                                }
                                if app.keybinds.toggle_sessions.matches(&key)
                                    && !app.input.starts_with('/')
                                {
                                    events::refresh_sessions(&agent, &mut app).await;
                                    app.show_session_picker = true;
                                    app.session_picker_selection = 0;
                                    continue;
                                }
                                if app.keybinds.toggle_reasoning.matches(&key) {
                                    app.show_reasoning = !app.show_reasoning;
                                    continue;
                                }

                                // ── view switching: Ctrl+1..5
                                if key.modifiers.contains(KeyModifiers::CONTROL) {
                                    match key.code {
                                        KeyCode::Char(c @ '1'..='5') => {
                                            if let Some(v) = View::from_index(c.to_digit(10).unwrap() as u8) {
                                                app.view = v;
                                            }
                                            continue;
                                        }
                                        // YYC-61: queue management hotkeys.
                                        // Ctrl+Backspace pops the most recent queued
                                        // submission; Ctrl+Shift+Backspace drops the
                                        // entire queue.
                                        KeyCode::Backspace => {
                                            if key.modifiers.contains(KeyModifiers::SHIFT) {
                                                app.queue.clear();
                                            } else {
                                                app.queue.pop_back();
                                            }
                                            continue;
                                        }
                                        // YYC-70: Ctrl+J / Ctrl+K navigate the
                                        // slash menu when it's open.
                                        KeyCode::Char('j') | KeyCode::Char('k') => {
                                            if app.input.starts_with('/') {
                                                let candidates = keymap::current_palette(&app.input);
                                                if !candidates.is_empty() {
                                                    let len = candidates.len();
                                                    if matches!(key.code, KeyCode::Char('j')) {
                                                        app.slash_menu_selection =
                                                            (app.slash_menu_selection + 1).min(len - 1);
                                                    } else {
                                                        app.slash_menu_selection =
                                                            app.slash_menu_selection.saturating_sub(1);
                                                    }
                                                    continue;
                                                }
                                            }
                                        }
                                        KeyCode::Char('c') => {
                                            match app.pop_cancel_scope() {
                                                CancelPop::Popped(_) => {
                                                    pending_quit = false;
                                                    continue;
                                                }
                                                CancelPop::CancelTurn => {
                                                    // YYC-105: fire the externally-held token directly
                                                    // rather than queueing a lock acquisition that
                                                    // would block on the in-flight prompt task. The
                                                    // agent mirror updates next iteration.
                                                    if let Some(c) = app.current_turn_cancel.as_ref() {
                                                        c.cancel();
                                                    }
                                                    app.messages.push(ChatMessage {
                                                        role: ChatRole::System,
                                                        content: "Cancelling current turn… (Ctrl+C again to quit)".into(),
                                                        reasoning: String::new(),
                                                        segments: Vec::new(),
                                                        render_version: 0,
                                                    });
                                                    pending_quit = true;
                                                    continue;
                                                }
                                                CancelPop::None if pending_quit => {
                                                    exit = true;
                                                }
                                                CancelPop::None => {
                                                    pending_quit = true;
                                                    app.messages.push(ChatMessage {
                                                        role: ChatRole::System,
                                                        content: "Press Ctrl+C again to quit, or any key to cancel.".into(),
                                                        reasoning: String::new(),
                                                        segments: Vec::new(),
                                                        render_version: 0,
                                                    });
                                                    continue;
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }

                                match key.code {
                                    KeyCode::Enter => {
                                        // YYC-70: when the slash menu is open with at least one
                                        // match, Enter commits the highlighted command.
                                        if app.input.starts_with('/') {
                                            let candidates = keymap::current_palette(&app.input);
                                            if !candidates.is_empty() {
                                                let idx = app.slash_menu_selection.min(candidates.len() - 1);
                                                app.prompt_set(format!("/{}", candidates[idx].name));
                                            }
                                        }
                                        match app.prompt_enter_intent() {
                                            PromptEnterIntent::Edit => {
                                                app.prompt_handle_key(key);
                                                pending_quit = false;
                                                continue;
                                            }
                                            PromptEnterIntent::Empty => continue,
                                            PromptEnterIntent::Submit(_) => {}
                                        }
                                        // YYC-62: classify slash commands as mid-turn-safe or
                                        // must-defer. Mid-turn-safe slash dispatch falls through
                                        // to the existing inline branch even while busy. Must-
                                        // defer slash commands emit a wait notice rather than
                                        // smuggling state-mutating ops past the agent loop.
                                        let is_slash = app.input.starts_with('/');
                                        let mid_turn_safe = if is_slash {
                                            let body = &app.input[1..];
                                            let head = body.split_whitespace().next().unwrap_or("");
                                            keymap::SLASH_COMMANDS
                                                .iter()
                                                .find(|c| c.name == head)
                                                .map(|c| c.mid_turn_safe)
                                                .or_else(|| {
                                                    app.frontend
                                                        .is_frontend_command_mid_turn_safe(head)
                                                })
                                                .unwrap_or(false)
                                        } else {
                                            false
                                        };
                                        if app.thinking && is_slash && !mid_turn_safe {
                                            let cmd_text = app.input.trim().to_string();
                                            app.prompt_clear();
                                            app.slash_menu_selection = 0;
                                            pending_quit = false;
                                            app.messages.push(ChatMessage {
                                                role: ChatRole::System,
                                                content: format!(
                                                    "{cmd_text} can't run while the agent is busy. Wait for the current turn to end (or Ctrl+C to cancel)."
                                                ),
                                                ..Default::default()
                                            });
                                            continue;
                                        }
                                        // YYC-61 → YYC-125: plain text during busy queues
                                        // as a "steer" — drained as a single batched
                                        // prompt at the next turn-end Done. Use /queue
                                        // for explicit FIFO post-turn scheduling.
                                        if !app.input.is_empty()
                                            && app.thinking
                                            && !is_slash
                                        {
                                            let msg = app.input.trim().to_string();
                                            if !msg.is_empty() {
                                                app.queue.push_back(msg.clone());
                                                app.messages.push(ChatMessage {
                                                    role: ChatRole::System,
                                                    content: format!(
                                                        "Steered (#{} pending): {msg}",
                                                        app.queue.len()
                                                    ),
                                                    ..Default::default()
                                                });
                                            }
                                            app.prompt_clear();
                                            app.slash_menu_selection = 0;
                                            pending_quit = false;
                                            continue;
                                        }
                                        // Idle, or busy + mid-turn-safe slash command — fall
                                        // through to dispatch.
                                        if !app.input.is_empty() {
                                            let msg = app.input.trim().to_string();
                                            app.prompt_clear();
                                            app.slash_menu_selection = 0;
                                            pending_quit = false;

                                            // slash commands
                                            if let Some(body) = msg.strip_prefix('/') {
                                                if let Some(dispatch) = app.frontend.dispatch_slash(&msg) {
                                                    apply_frontend_dispatch(&mut app, dispatch);
                                                    continue;
                                                }
                                                match body {
                                                    "exit" | "quit" => { exit = true; continue; }
                                                    "help" => {
                                                        let mut help = String::from("Commands:");
                                                        for cmd in keymap::SLASH_COMMANDS {
                                                            help.push_str(&format!("\n  /{:<10}  {}", cmd.name, cmd.description));
                                                        }
                                                        for cmd in app.frontend.command_specs() {
                                                            help.push_str(&format!("\n  /{:<10}  {}", cmd.name, cmd.description));
                                                        }
                                                        help.push_str("\n\nKeys:\n  Ctrl+1..5  switch view (1=stack 2=split 3=tiled 4=tree 5=floor)\n  Ctrl+R     toggle reasoning trace\n  Tab        complete slash command");
                                                        app.messages.push(ChatMessage { role: ChatRole::System, content: help, ..Default::default() });
                                                        continue;
                                                    }
                                                    "clear" => {
                                                        app.messages.clear();
                                                        app.chat_render_store.borrow_mut().clear();
                                                        continue;
                                                    }
                                                    "reasoning" => {
                                                        app.show_reasoning = !app.show_reasoning;
                                                        app.messages.push(ChatMessage {
                                                            role: ChatRole::System,
                                                            content: format!("Reasoning trace: {}", if app.show_reasoning { "on" } else { "off" }),
                                                            reasoning: String::new(),
                            segments: Vec::new(),
                                                            render_version: 0,
                                                        });
                                                        continue;
                                                    }
                                                    "view" => {
                                                        let next = match app.view {
                                                            View::SingleStack => View::SplitSessions,
                                                            View::SplitSessions => View::TiledMesh,
                                                            View::TiledMesh => View::TreeOfThought,
                                                            View::TreeOfThought => View::TradingFloor,
                                                            View::TradingFloor => View::SingleStack,
                                                        };
                                                        app.view = next;
                                                        continue;
                                                    }
                                                    s if s.starts_with("diff-style") => {
                                                        let arg = s["diff-style".len()..].trim();
                                                        let next = if arg.is_empty() {
                                                            app.diff_style.next()
                                                        } else {
                                                            match crate::tui::state::DiffStyle::parse(arg) {
                                                                Some(d) => d,
                                                                None => {
                                                                    app.messages.push(ChatMessage {
                                                                        role: ChatRole::System,
                                                                        content: format!(
                                                                            "Unknown diff style '{arg}'. Use unified | side-by-side | inline."
                                                                        ),
                                                                        ..Default::default()
                                                                    });
                                                                    continue;
                                                                }
                                                            }
                                                        };
                                                        app.diff_style = next;
                                                        app.messages.push(ChatMessage {
                                                            role: ChatRole::System,
                                                            content: format!(
                                                                "Diff style: {}", next.label()
                                                            ),
                                                            ..Default::default()
                                                        });
                                                        continue;
                                                    }
                                                    s if s.starts_with("view ") => {
                                                        if let Ok(n) = s[5..].trim().parse::<u8>()
                                                            && let Some(v) = View::from_index(n) {
                                                                app.view = v;
                                                            }
                                                        continue;
                                                    }
                                                    "resume" => {
                                                        events::refresh_sessions(&agent, &mut app).await;
                                                        app.show_session_picker = true;
                                                        app.session_picker_selection = 0;
                                                        continue;
                                                    }
                                                    s if s.starts_with("queue ") => {
                                                        // YYC-125: defer until after the steer batch.
                                                        let body = s[6..].trim();
                                                        if body.is_empty() {
                                                            app.messages.push(ChatMessage {
                                                                role: ChatRole::System,
                                                                content: "Usage: /queue <msg> — schedules a prompt to run after the next turn ends and any pending steers flush.".into(),
                                                                ..Default::default()
                                                            });
                                                            continue;
                                                        }
                                                        app.deferred_queue.push_back(body.to_string());
                                                        app.messages.push(ChatMessage {
                                                            role: ChatRole::System,
                                                            content: format!(
                                                                "Queued (#{}, deferred): {body}",
                                                                app.deferred_queue.len()
                                                            ),
                                                            ..Default::default()
                                                        });
                                                        continue;
                                                    }
                                                    "skills" => {
                                                        let body = {
                                                            let a = agent.lock().await;
                                                            let skills = a.skills();
                                                            if skills.is_empty() {
                                                                "No skills loaded.".to_string()
                                                            } else {
                                                                let mut out = format!(
                                                                    "Loaded skills ({}):",
                                                                    skills.len()
                                                                );
                                                                for s in skills {
                                                                    out.push_str(&format!(
                                                                        "\n  • {} — {}",
                                                                        s.name, s.description
                                                                    ));
                                                                }
                                                                out
                                                            }
                                                        };
                                                        app.messages.push(ChatMessage {
                                                            role: ChatRole::System,
                                                            content: body,
                                                            ..Default::default()
                                                        });
                                                        continue;
                                                    }
                                                    "queue" => {
                                                        app.messages.push(ChatMessage {
                                                            role: ChatRole::System,
                                                            content: "Usage: /queue <msg> — schedules a prompt to run after the next turn ends and any pending steers flush.".into(),
                                                            ..Default::default()
                                                        });
                                                        continue;
                                                    }
                                                    "status" => {
                                                        let (session_id, profile, ctx_used) = {
                                                            let a = agent.lock().await;
                                                            (
                                                                a.session_id().to_string(),
                                                                a.active_profile()
                                                                    .map(str::to_string),
                                                                a.max_context() as u32,
                                                            )
                                                        };
                                                        let profile_label = profile
                                                            .as_deref()
                                                            .unwrap_or("[provider]");
                                                        let body = format!(
                                                            "Session: {}\nProvider: {}\nModel: {}\nContext window: {}\nLast prompt: {} tokens · session totals: {} prompt / {} completion · {} tool calls ({} errors)",
                                                            short_id(&session_id),
                                                            profile_label,
                                                            app.model_label,
                                                            crate::tui::state::format_thousands(ctx_used),
                                                            crate::tui::state::format_thousands(app.prompt_tokens_last),
                                                            crate::tui::state::format_thousands(app.prompt_tokens_total),
                                                            crate::tui::state::format_thousands(app.completion_tokens_total),
                                                            app.tool_calls_total,
                                                            app.tool_errors_total,
                                                        );
                                                        app.messages.push(ChatMessage {
                                                            role: ChatRole::System,
                                                            content: body,
                                                            ..Default::default()
                                                        });
                                                        continue;
                                                    }
                                                    s if s.starts_with("search ") => {
                                                        let query = s[7..].trim();
                                                        if query.is_empty() {
                                                            app.messages.push(ChatMessage {
                                                                role: ChatRole::System,
                                                                content: "Usage: /search <query>".into(),
                                                                reasoning: String::new(),
                            segments: Vec::new(),
                                                                render_version: 0,
                                                            });
                                                            continue;
                                                        }
                                                        let hits = {
                                                            let a = agent.lock().await;
                                                            a.memory().search_messages(query, 10)
                                                        };
                                                        let report = match hits {
                                                            Ok(hs) if hs.is_empty() => format!("No matches for '{query}'"),
                                                            Ok(hs) => {
                                                                let mut out = format!("Search '{query}' — {} hit(s):", hs.len());
                                                                for h in hs {
                                                                    let preview: String = h.content.chars().take(100).collect();
                                                                    out.push_str(&format!(
                                                                        "\n  [{}] {} — {}",
                                                                        short_id(&h.session_id),
                                                                        h.role,
                                                                        preview.replace('\n', " ")
                                                                    ));
                                                                }
                                                                out
                                                            }
                                                            Err(e) => format!("Search failed: {e}"),
                                                        };
                                                        app.messages.push(ChatMessage {
                                                            role: ChatRole::System,
                                                            content: report,
                                                            reasoning: String::new(),
                            segments: Vec::new(),
                                                            render_version: 0,
                                                        });
                                                        continue;
                                                    }
                                                    s if s == "provider" || s.starts_with("provider ") => {
                                                        let arg = s["provider".len()..].trim();
                                                        if arg.is_empty() {
                                                            // YYC-97: arrow-key picker overlay.
                                                            let active = {
                                                                let a = agent.lock().await;
                                                                a.active_profile().map(str::to_string)
                                                            };
                                                            let items =
                                                                keymap::build_provider_picker_entries(config);
                                                            let active_idx = items
                                                                .iter()
                                                                .position(|e| e.name == active)
                                                                .unwrap_or(0);
                                                            app.open_provider_picker(items, active_idx);
                                                            continue;
                                                        }

                                                        let target: Option<&str> = if arg.eq_ignore_ascii_case("default") {
                                                            None
                                                        } else {
                                                            Some(arg)
                                                        };
                                                        let result = {
                                                            let mut a = agent.lock().await;
                                                            a.switch_provider(target, config).await
                                                        };
                                                        match result {
                                                            Ok(selection) => {
                                                                app.model_label = selection.model.id.clone();
                                                                app.token_max = selection.max_context as u32;
                                                                app.pricing = selection.pricing;
                                                                app.provider_label = target.map(str::to_string);
                                                                let label = target.unwrap_or("default");
                                                                app.messages.push(ChatMessage {
                                                                    role: ChatRole::System,
                                                                    content: format!(
                                                                        "Provider switched to {label} · {} · context {}",
                                                                        app.model_label,
                                                                        crate::tui::state::format_thousands(app.token_max),
                                                                    ),
                                                                    ..Default::default()
                                                                });
                                                            }
                                                            Err(e) => {
                                                                app.messages.push(ChatMessage {
                                                                    role: ChatRole::System,
                                                                    content: format!("Provider switch failed: {e}"),
                                                                    ..Default::default()
                                                                });
                                                            }
                                                        }
                                                        continue;
                                                    }
                                                    s if s == "model" || s.starts_with("model ") => {
                                                        let arg = s["model".len()..].trim();
                                                        if arg.is_empty() {
                                                            // YYC-97: arrow-key picker overlay.
                                                            let (models_result, active) = {
                                                                let a = agent.lock().await;
                                                                (
                                                                    a.available_models().await,
                                                                    a.active_model().to_string(),
                                                                )
                                                            };
                                                            match models_result {
                                                                Ok(models) if models.is_empty() => {
                                                                    app.messages.push(ChatMessage {
                                                                        role: ChatRole::System,
                                                                        content: "Provider catalog returned no models.".into(),
                                                                        ..Default::default()
                                                                    });
                                                                }
                                                                Ok(active_models) => {
                                                                    rendering::open_unified_picker(
                                                                        &mut app,
                                                                        config,
                                                                        &agent,
                                                                        &active,
                                                                        active_models,
                                                                    )
                                                                    .await;
                                                                }
                                                                Err(e) => {
                                                                    app.messages.push(ChatMessage {
                                                                        role: ChatRole::System,
                                                                        content: format!("Model catalog fetch failed: {e}"),
                                                                        ..Default::default()
                                                                    });
                                                                }
                                                            }
                                                            continue;
                                                        }

                                                        let result = {
                                                            let mut a = agent.lock().await;
                                                            a.switch_model(arg).await
                                                        };
                                                        match result {
                                                            Ok(selection) => {
                                                                app.model_label = selection.model.id.clone();
                                                                app.token_max = selection.max_context as u32;
                                                                app.pricing = selection.pricing;
                                                                app.messages.push(ChatMessage {
                                                                    role: ChatRole::System,
                                                                    content: format!(
                                                                        "Model switched to {} · context {}",
                                                                        app.model_label,
                                                                        crate::tui::state::format_thousands(app.token_max),
                                                                    ),
                                                                    ..Default::default()
                                                                });
                                                            }
                                                            Err(e) => {
                                                                app.messages.push(ChatMessage {
                                                                    role: ChatRole::System,
                                                                    content: format!("Model switch failed: {e}"),
                                                                    ..Default::default()
                                                                });
                                                            }
                                                        }
                                                        continue;
                                                    }
                                                    _ => {
                                                        app.messages.push(ChatMessage {
                                                            role: ChatRole::System,
                                                            content: format!("Unknown command: {msg}. Try /help"),
                                                            reasoning: String::new(),
                            segments: Vec::new(),
                                                            render_version: 0,
                                                        });
                                                        continue;
                                                    }
                                                }
                                            }

                                            events::submit_prompt(&mut app, &agent, &stream_tx, msg);
                                        }
                                    }
                                    KeyCode::Char(_) => {
                                        pending_quit = false;
                                        app.prompt_handle_key(key);
                                        // Re-filtering may shrink the menu; keep the highlight
                                        // anchored at the top so the visible top row is selected.
                                        app.slash_menu_selection = 0;
                                    }
                                    KeyCode::Backspace => {
                                        pending_quit = false;
                                        app.prompt_handle_key(key);
                                        app.slash_menu_selection = 0;
                                    }
                                    KeyCode::Tab => {
                                        pending_quit = false;
                                        if let Some(rest) = app.input.strip_prefix('/')
                                            && let Some(c) = keymap::complete_slash(rest) {
                                                app.prompt_set(format!("/{c}"));
                                            } else {
                                                app.prompt_handle_key(key);
                                            }
                                    }
                                    KeyCode::Up => {
                                        // YYC-70: arrows navigate the slash menu when open.
                                        if app.input.starts_with('/') {
                                            let candidates = keymap::current_palette(&app.input);
                                            if !candidates.is_empty() {
                                                app.slash_menu_selection =
                                                    app.slash_menu_selection.saturating_sub(1);
                                                continue;
                                            }
                                        }
                                        if !app.input.is_empty() && app.prompt_handle_key(key) {
                                            pending_quit = false;
                                            continue;
                                        }
                                        // YYC-123: 3 lines per arrow keypress so
                                        // holding Up/Down feels closer to a
                                        // mouse-wheel scroll instead of crawling
                                        // one line at a time per render frame.
                                        app.scroll = app.scroll.saturating_sub(3);
                                        app.at_bottom = false;
                                    }
                                    KeyCode::Down => {
                                        if app.input.starts_with('/') {
                                            let candidates = keymap::current_palette(&app.input);
                                            if !candidates.is_empty() {
                                                let len = candidates.len();
                                                app.slash_menu_selection =
                                                    (app.slash_menu_selection + 1).min(len - 1);
                                                continue;
                                            }
                                        }
                                        if !app.input.is_empty() && app.prompt_handle_key(key) {
                                            pending_quit = false;
                                            continue;
                                        }
                                        let max = app.chat_max_scroll.get();
                                        app.scroll = app.scroll.saturating_add(3).min(max);
                                        app.at_bottom = app.scroll >= max;
                                    }
                                    KeyCode::Left
                                    | KeyCode::Right
                                    | KeyCode::Home
                                    | KeyCode::End
                                    | KeyCode::Delete => {
                                        pending_quit = false;
                                        app.prompt_handle_key(key);
                                    }
                                    KeyCode::PageUp => {
                                        app.scroll = app.scroll.saturating_sub(10);
                                        app.at_bottom = false;
                                    }
                                    KeyCode::PageDown => {
                                        let max = app.chat_max_scroll.get();
                                        app.scroll = app.scroll.saturating_add(10).min(max);
                                        app.at_bottom = app.scroll >= max;
                                    }
                                    KeyCode::Esc => {
                                        // YYC-58: in Command mode (slash buffer pending), Esc
                                        // clears the buffer and drops back to Insert; only Esc
                                        // with an empty buffer exits.
                                        match app.prompt_escape_intent() {
                                            PromptEscapeIntent::ClearCommand => {
                                                app.prompt_clear();
                                                app.slash_menu_selection = 0;
                                            }
                                            PromptEscapeIntent::Edit => {
                                                app.prompt_handle_key(key);
                                            }
                                            PromptEscapeIntent::Exit => {
                                                exit = true;
                                            }
                                        }
                                    }
                                    _ => { pending_quit = false; }
                                }
                            }
                    }
                    Some(KeyEv::Error(e)) => {
                        tracing::error!("Terminal input error: {e}");
                        exit = true;
                    }
                    None => exit = true,
                }
            }
            ev = stream_rx.recv() => {
                match ev {
                    Some(ev) => {
                        let mut force_redraw = events::stream_event_forces_redraw(&ev);
                        events::handle_stream_event(&mut app, &agent, &stream_tx, ev).await;
                        while let Ok(ev) = stream_rx.try_recv() {
                            force_redraw |= events::stream_event_forces_redraw(&ev);
                            events::handle_stream_event(&mut app, &agent, &stream_tx, ev).await;
                        }
                        if !force_redraw
                            && let events::RenderWake::Wait(delay) =
                                events::render_wake_for_stream_batch(last_draw, Instant::now(), false)
                            {
                                tokio::time::sleep(delay).await;
                            }
                    }
                    None => {
                        app.thinking = false;
                        app.current_turn_cancel = None;
                    }
                }
            }
            frontend_event = frontend_event_rx.recv() => {
                match frontend_event {
                    Ok(event) => {
                        let updates = app.frontend.handle_extension_event(&serde_json::json!({
                            "kind": "extension_event",
                            "session_id": event.session_id,
                            "extension_id": event.extension_id,
                            "payload": event.payload,
                        }));
                        app.apply_widget_updates(updates);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                }
            }
        }
    }

    // ── End the session before tearing down the terminal so SessionEnd hooks
    // see the final state.
    {
        let a = agent.lock().await;
        a.end_session().await;
    }

    init::restore_terminal()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn stream_batching_caps_stream_redraws_to_frame_budget() {
        let start = Instant::now();

        assert_eq!(
            events::render_wake_for_stream_batch(start, start + Duration::from_millis(1), false),
            events::RenderWake::Wait(Duration::from_millis(15))
        );
    }

    #[test]
    fn input_events_render_immediately() {
        let start = Instant::now();

        assert_eq!(
            events::render_wake_for_stream_batch(start, start + Duration::from_millis(1), true),
            events::RenderWake::Now
        );
    }

    #[test]
    fn model_command_is_available_and_deferred_mid_turn() {
        let command = keymap::SLASH_COMMANDS
            .iter()
            .find(|cmd| cmd.name == "model")
            .expect("model slash command");

        assert!(!command.mid_turn_safe);
        assert_eq!(keymap::filter_commands("mod")[0].name, "model");
    }

    #[test]
    fn build_provider_picker_entries_lists_default_first_then_named_sorted() {
        use crate::config::{Config, ProviderConfig};
        use std::collections::HashMap;

        let mut providers = HashMap::new();
        let mut local = ProviderConfig::default();
        local.base_url = "http://localhost:11434/v1".into();
        local.model = "qwen2.5".into();
        providers.insert("local".into(), local);
        let mut alpha = ProviderConfig::default();
        alpha.base_url = "https://alpha.example".into();
        alpha.model = "alpha-1".into();
        providers.insert("alpha".into(), alpha);

        let mut config = Config::default();
        config.provider.base_url = "https://openrouter.ai/api/v1".into();
        config.provider.model = "deepseek/v4".into();
        config.providers = providers;

        let entries = keymap::build_provider_picker_entries(&config);
        assert_eq!(entries.len(), 3);
        assert!(entries[0].name.is_none());
        assert_eq!(entries[0].model, "deepseek/v4");
        assert_eq!(entries[1].name.as_deref(), Some("alpha"));
        assert_eq!(entries[2].name.as_deref(), Some("local"));
    }

    #[test]
    fn provider_command_is_available_and_deferred_mid_turn() {
        let command = keymap::SLASH_COMMANDS
            .iter()
            .find(|cmd| cmd.name == "provider")
            .expect("provider slash command");

        assert!(!command.mid_turn_safe);
        assert_eq!(keymap::filter_commands("prov")[0].name, "provider");
    }

    #[test]
    fn format_provider_list_marks_active_profile_and_lists_named() {
        use crate::config::{Config, ProviderConfig};
        use std::collections::HashMap;

        let mut providers = HashMap::new();
        let mut local = ProviderConfig::default();
        local.base_url = "http://localhost:11434/v1".into();
        local.model = "qwen2.5".into();
        providers.insert("local".into(), local);

        let mut config = Config::default();
        config.provider.base_url = "https://openrouter.ai/api/v1".into();
        config.provider.model = "deepseek/v4".into();
        config.providers = providers;

        let active_default = keymap::format_provider_list(&config, None);
        assert!(active_default.contains("* default · https://openrouter.ai/api/v1 · deepseek/v4"));
        assert!(active_default.contains("  local · http://localhost:11434/v1 · qwen2.5"));

        let active_local = keymap::format_provider_list(&config, Some("local"));
        assert!(active_local.contains("  default · https://openrouter.ai/api/v1"));
        assert!(active_local.contains("* local · http://localhost:11434/v1"));
    }

    #[test]
    fn format_provider_list_handles_no_named_profiles() {
        use crate::config::Config;
        let config = Config::default();
        let report = keymap::format_provider_list(&config, None);
        assert!(report.contains("* default"));
        assert!(report.contains("(no named [providers.<name>] profiles configured)"));
    }

    #[test]
    fn format_model_list_marks_active_model() {
        let models = vec![
            crate::provider::catalog::ModelInfo {
                id: "model-a".into(),
                display_name: "Model A".into(),
                context_length: 1_000,
                pricing: None,
                features: crate::provider::catalog::ModelFeatures {
                    tools: true,
                    vision: false,
                    json_mode: true,
                    reasoning: false,
                },
                top_provider: None,
            },
            crate::provider::catalog::ModelInfo {
                id: "model-b".into(),
                display_name: "Model B".into(),
                context_length: 0,
                pricing: None,
                features: crate::provider::catalog::ModelFeatures::default(),
                top_provider: None,
            },
        ];

        let report = keymap::format_model_list("model-a", &models);

        assert!(report.contains("* model-a · ctx 1,000 · tools,json"));
        assert!(report.contains("  model-b · ctx unknown"));
        assert!(report.contains("Use /model <id> to switch."));
    }
}
