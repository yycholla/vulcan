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
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::{layout::Rect, prelude::Position};
use tokio::sync::{Mutex, mpsc};

use crate::agent::Agent;
use crate::config::Config;
use crate::hooks::HookRegistry;
use crate::hooks::audit::AuditHook;
use crate::pause::{self, PauseKind};
use crate::provider::StreamEvent;

pub mod chat_message;
pub mod chat_render;
pub mod diff_scrubber;
pub mod effects;
mod events;
mod focus;
pub mod frontend;
mod init;
pub mod input;
pub mod keybinds;
mod keymap;
mod layouts;
pub mod markdown;
pub mod miller_columns;
pub mod model_picker;
pub mod orchestration;
pub mod pause_prompt;
pub mod picker_state;
pub mod prompt;
pub mod provider_picker;
mod rendering;
pub mod state;
mod surface;
mod surface_events;
pub mod theme;
mod ui_runtime;
pub mod views;
pub mod widgets;

use state::{AppState, CancelPop, ChatMessage, ChatRole, PromptEnterIntent, PromptEscapeIntent};
use theme::{Theme, body};
use views::{View, render_view};
use vulcan_frontend_api::{CanvasKey, FrontendCommandAction};

use self::input::{TuiInputEvent, TuiKeyCode, TuiKeyEvent, TuiKeyModifiers, TuiMouseEventKind};

const MOTION_FRAME_BUDGET: Duration = Duration::from_millis(120);

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

pub(super) enum KeyEv {
    Press(TuiInputEvent),
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
        FrontendCommandAction::OpenSurface(surface) => {
            app.open_frontend_surface(surface);
        }
        FrontendCommandAction::UpdateSurface(update) => {
            app.update_frontend_surface(update);
        }
        FrontendCommandAction::CloseSurface { id } => {
            app.close_frontend_surface(id);
        }
        FrontendCommandAction::OpenView { id, title, body } => {
            app.open_frontend_surface(vulcan_frontend_api::FrontendSurface::modal(id, title, body));
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
    for surface in dispatch.surface_requests {
        app.open_frontend_surface(surface);
    }
    for update in dispatch.surface_updates {
        app.update_frontend_surface(update);
    }
    for id in dispatch.surface_closes {
        app.close_frontend_surface(id);
    }
    for request in dispatch.canvas_requests {
        app.install_canvas_request(request);
    }
}

fn canvas_key_from_event(key: TuiKeyEvent) -> CanvasKey {
    if key.modifiers.contains(TuiKeyModifiers::CONTROL) && matches!(key.code, TuiKeyCode::Char('c'))
    {
        return CanvasKey::CtrlC;
    }
    match key.code {
        TuiKeyCode::Up => CanvasKey::Up,
        TuiKeyCode::Down => CanvasKey::Down,
        TuiKeyCode::Left => CanvasKey::Left,
        TuiKeyCode::Right => CanvasKey::Right,
        TuiKeyCode::Esc => CanvasKey::Esc,
        TuiKeyCode::Enter => CanvasKey::Enter,
        TuiKeyCode::Backspace => CanvasKey::Backspace,
        TuiKeyCode::Char(c) => CanvasKey::Char(c),
        other => CanvasKey::Other(format!("{other:?}")),
    }
}

pub async fn run_tui(
    config: &Config,
    resume: ResumeTarget,
    tool_profile: Option<String>,
) -> Result<()> {
    let terminal_session = init::init_terminal()?;
    tracing::debug!(
        ?terminal_session.capabilities,
        "detected terminal capabilities"
    );
    let mut terminal = terminal_session.terminal;

    // keyboard
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<KeyEv>();
    let tx_keys = key_tx.clone();
    std::thread::spawn(move || {
        let mut input_terminal = match init::init_input_terminal() {
            Ok(terminal) => terminal,
            Err(e) => {
                let _ = tx_keys.send(KeyEv::Error(e.to_string()));
                return;
            }
        };
        loop {
            match init::read_input_event(&mut input_terminal) {
                Ok(Some(ev)) => {
                    if tx_keys.send(KeyEv::Press(TuiInputEvent::from(ev))).is_err() {
                        break;
                    }
                }
                Ok(None) => {}
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
    if matches!(resume, ResumeTarget::Pick) {
        app.open_session_picker(0);
    }

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
    let mut last_motion_frame = Instant::now();

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
        app.effects.prepare_frame();
        let now = Instant::now();
        if app.activity_motion_active()
            && now.saturating_duration_since(last_motion_frame) >= MOTION_FRAME_BUDGET
        {
            app.advance_activity_motion();
            last_motion_frame = now;
        }

        // YYC-69: keep the chat viewport pinned to the latest content while
        // the user hasn't scrolled away. `chat_max_scroll` is published by
        // the renderer on the previous frame, so this lags one tick after a
        // new event — invisible in practice.
        if app.at_bottom {
            app.scroll = app.chat_max_scroll.get();
        }

        let draw_started = Instant::now();
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

            rendering::draw_surface_overlays(f, area, &app);
            rendering::draw_diagnostics(f, area, &app);
        })?;
        app.note_frame_draw(draw_started.elapsed());
        last_draw = Instant::now();
        if app.finish_chat_clear_if_idle() {
            continue;
        }

        // ── Diff scrubber overlay (YYC-75): intercept input until resolved.
        if app.has_diff_scrubber() {
            surface_events::drive_diff_scrubber(&mut app, &mut key_rx).await;
            continue;
        }

        // ── Hierarchical model picker (YYC-101): miller columns,
        // hjkl drill-down. Intercepts input until dismissed.
        if app.has_model_picker() {
            tokio::select! {
                _ = surface_events::drive_model_picker(&mut app, &agent, config, &mut key_rx) => {}
                _ = tokio::time::sleep(MOTION_FRAME_BUDGET), if app.activity_motion_active() => {}
            }
            continue;
        }

        if app.has_text_surface() {
            match key_rx.recv().await {
                Some(KeyEv::Press(TuiInputEvent::Key(key))) => {
                    app.handle_surface_key(canvas_key_from_event(key));
                }
                Some(KeyEv::Press(_)) => {}
                Some(KeyEv::Error(e)) => {
                    tracing::error!("Terminal input error (frontend surface): {e}");
                    app.close_text_surface();
                }
                None => {
                    app.close_text_surface();
                }
            }
            continue;
        }

        // ── Provider picker overlay (YYC-97): intercept input until dismissed.
        if app.has_provider_picker() {
            surface_events::drive_provider_picker(&mut app, &agent, config, &mut key_rx).await;
            continue;
        }

        // ── Session picker mode: intercept all input until dismissed.
        if app.has_session_picker() {
            surface_events::drive_session_picker(
                &mut app,
                &agent,
                config,
                &mut key_rx,
                &mut pause_rx,
                &pause_tx,
            )
            .await;
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
                        app.open_diff_scrubber(path.clone(), hunks.clone(), p);
                        continue;
                    }
                    let summary = pause_prompt::pause_summary(&p);
                    app.messages.push(ChatMessage {
                        role: ChatRole::System,
                        content: format!("⏸  Agent paused — {summary}"),
                        reasoning: String::new(),
                            segments: Vec::new(),
                        render_version: 0,
                    });
                    app.note_pause(&summary);
                    app.open_pause_prompt(summary, p);
                }
                continue;
            }
            ev = key_rx.recv() => {
                match ev {
                    Some(KeyEv::Press(TuiInputEvent::Paste(text))) => {
                        // YYC-124: bracketed-paste payload — terminals that
                        // support CSI 200~/201~ deliver the whole pasted
                        // buffer as one paste event instead of N
                        // separate TuiKeyCode::Char events. Append the chunk
                        // to the input buffer in one shot; embedded
                        // newlines stay literal so multiline pastes don't
                        // submit a prompt per line. Skipped while a pause
                        // overlay is active so the paste can't smuggle
                        // text into the resume keystroke handler.
                        if !app.has_pause_prompt() {
                            app.prompt_insert_str(&text);
                        }
                        continue;
                    }
                    Some(KeyEv::Press(TuiInputEvent::Mouse(m))) => {
                        // YYC-123: mouse wheel drives the chat viewport
                        // directly. Three lines per notch matches a
                        // typical terminal scroll feel; PageUp/PageDown
                        // remain available for bigger jumps.
                        const SCROLL_LINES: u16 = 3;
                        match m.kind {
                            TuiMouseEventKind::ScrollUp => {
                                app.scroll = app.scroll.saturating_sub(SCROLL_LINES);
                                app.at_bottom = false;
                            }
                            TuiMouseEventKind::ScrollDown => {
                                let max = app.chat_max_scroll.get();
                                app.scroll = app.scroll.saturating_add(SCROLL_LINES).min(max);
                                app.at_bottom = app.scroll >= max;
                            }
                            TuiMouseEventKind::Other => {}
                        }
                        continue;
                    }
                    Some(KeyEv::Press(
                        TuiInputEvent::Resize { .. }
                        | TuiInputEvent::Wake
                        | TuiInputEvent::Unsupported,
                    )) => {
                        continue;
                    }
                    Some(KeyEv::Press(TuiInputEvent::Key(key))) => {
                                if app.has_active_canvas() {
                                    app.handle_canvas_key(canvas_key_from_event(key));
                                    pending_quit = false;
                                    continue;
                                }

                                // ── If a pause is active, intercept the keys that
                                // dispatch a response. Anything else falls through
                                // to normal handling so the user can still scroll, etc.
                                if app.has_pause_prompt() {
                                    let outcome = app.handle_pause_prompt_key(key);
                                    if let (Some(p), Some(resume), Some(label)) =
                                        (outcome.pause, outcome.resume, outcome.label)
                                    {
                                        let _ = p.reply.send(resume);
                                        app.messages.push(ChatMessage {
                                            role: ChatRole::System,
                                            content: format!("▶  Resumed — {label}"),
                                            reasoning: String::new(),
                                            segments: Vec::new(),
                                            render_version: 0,
                                        });
                                        app.note_resume(label);
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
                                    app.open_session_picker(0);
                                    continue;
                                }
                                if app.keybinds.toggle_reasoning.matches(&key) {
                                    app.show_reasoning = !app.show_reasoning;
                                    continue;
                                }

                                // ── view switching: Ctrl+1..5
                                if key.modifiers.contains(TuiKeyModifiers::CONTROL) {
                                    match key.code {
                                        TuiKeyCode::Char(c @ '1'..='5') => {
                                            if let Some(v) = View::from_index(c.to_digit(10).unwrap() as u8) {
                                                app.view = v;
                                            }
                                            continue;
                                        }
                                        // YYC-61: queue management hotkeys.
                                        // Ctrl+Backspace pops the most recent queued
                                        // submission; Ctrl+Shift+Backspace drops the
                                        // entire queue.
                                        TuiKeyCode::Backspace => {
                                            if key.modifiers.contains(TuiKeyModifiers::SHIFT) {
                                                app.queue.clear();
                                            } else {
                                                app.queue.pop_back();
                                            }
                                            continue;
                                        }
                                        // YYC-70: Ctrl+J / Ctrl+K navigate the
                                        // slash menu when it's open.
                                        TuiKeyCode::Char('j') | TuiKeyCode::Char('k') => {
                                            if app.input.starts_with('/') {
                                                let candidates = keymap::current_palette(&app.input);
                                                if !candidates.is_empty() {
                                                    let len = candidates.len();
                                                    if matches!(key.code, TuiKeyCode::Char('j')) {
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
                                        TuiKeyCode::Char('c') => {
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
                                    TuiKeyCode::Enter => {
                                        // YYC-70: when the slash menu is open with at least one
                                        // match, Enter commits the highlighted command.
                                        if app.input.starts_with('/') {
                                            let candidates = keymap::current_palette(&app.input);
                                            if !candidates.is_empty() {
                                                let idx = app.slash_menu_selection.min(candidates.len() - 1);
                                                app.prompt_set(format!("/{}", candidates[idx].name));
                                            }
                                        }
                                        if key.modifiers.contains(TuiKeyModifiers::SHIFT) {
                                            app.prompt_handle_key(key);
                                            pending_quit = false;
                                            continue;
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
                                                        app.request_chat_clear();
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
                                                        app.open_session_picker(0);
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
                                                    "diagnostics" => {
                                                        app.toggle_diagnostics();
                                                        app.messages.push(ChatMessage {
                                                            role: ChatRole::System,
                                                            content: format!(
                                                                "Diagnostics overlay: {}",
                                                                if app.show_diagnostics { "on" } else { "off" }
                                                            ),
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
                                    TuiKeyCode::Char(_) => {
                                        pending_quit = false;
                                        app.prompt_handle_key(key);
                                        // Re-filtering may shrink the menu; keep the highlight
                                        // anchored at the top so the visible top row is selected.
                                        app.slash_menu_selection = 0;
                                    }
                                    TuiKeyCode::Backspace => {
                                        pending_quit = false;
                                        app.prompt_handle_key(key);
                                        app.slash_menu_selection = 0;
                                    }
                                    TuiKeyCode::Tab => {
                                        pending_quit = false;
                                        if let Some(rest) = app.input.strip_prefix('/')
                                            && let Some(c) = keymap::complete_slash(rest) {
                                                app.prompt_set(format!("/{c}"));
                                            } else {
                                                app.prompt_handle_key(key);
                                            }
                                    }
                                    TuiKeyCode::Up => {
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
                                    TuiKeyCode::Down => {
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
                                    TuiKeyCode::Left
                                    | TuiKeyCode::Right
                                    | TuiKeyCode::Home
                                    | TuiKeyCode::End
                                    | TuiKeyCode::Delete => {
                                        pending_quit = false;
                                        app.prompt_handle_key(key);
                                    }
                                    TuiKeyCode::PageUp => {
                                        app.scroll = app.scroll.saturating_sub(10);
                                        app.at_bottom = false;
                                    }
                                    TuiKeyCode::PageDown => {
                                        let max = app.chat_max_scroll.get();
                                        app.scroll = app.scroll.saturating_add(10).min(max);
                                        app.at_bottom = app.scroll >= max;
                                    }
                                    TuiKeyCode::Esc => {
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
                        let dispatch = app.frontend.handle_extension_event(&serde_json::json!({
                            "kind": "extension_event",
                            "session_id": event.session_id,
                            "extension_id": event.extension_id,
                            "payload": event.payload,
                        }));
                        if let Some(dispatch) = dispatch {
                            apply_frontend_dispatch(&mut app, dispatch);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                }
            }
            _ = tokio::time::sleep(MOTION_FRAME_BUDGET), if app.activity_motion_active() => {}
        }
    }

    // ── End the session before tearing down the terminal so SessionEnd hooks
    // see the final state.
    {
        let a = agent.lock().await;
        a.end_session().await;
    }

    drop(terminal);
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
