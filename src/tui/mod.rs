use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::{
    Terminal,
    crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    layout::Rect,
    prelude::Position,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use tokio::sync::{Mutex, mpsc};

use crate::agent::Agent;
use crate::config::Config;
use crate::hooks::HookRegistry;
use crate::hooks::audit::AuditHook;
use crate::pause::{self, AgentResume, PauseKind};
use crate::provider::StreamEvent;

pub mod chat_render;
pub mod keybinds;
pub mod markdown;
pub mod model_picker;
pub mod state;
pub mod theme;
pub mod views;
pub mod widgets;

use state::{AppState, ChatMessage, ChatRole, SessionStatus};
use theme::{Theme, body};
use views::{View, render_view};

const STREAM_FRAME_BUDGET: Duration = Duration::from_millis(16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RenderWake {
    Now,
    Wait(Duration),
}

fn render_wake_for_stream_batch(
    last_draw: Instant,
    now: Instant,
    is_terminal_event: bool,
) -> RenderWake {
    if is_terminal_event {
        return RenderWake::Now;
    }

    let elapsed = now.saturating_duration_since(last_draw);
    if elapsed >= STREAM_FRAME_BUDGET {
        RenderWake::Now
    } else {
        RenderWake::Wait(STREAM_FRAME_BUDGET - elapsed)
    }
}

fn stream_event_forces_redraw(ev: &StreamEvent) -> bool {
    matches!(ev, StreamEvent::Done(_) | StreamEvent::Error(_))
}

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

#[derive(Debug, Clone)]
struct SlashCommand {
    name: &'static str,
    description: &'static str,
    /// True when the command can run mid-turn without corrupting agent state
    /// (YYC-62). Pure UI ops are safe; anything that mutates conversation
    /// history or reaches into the agent is not. Default false (conservative).
    mid_turn_safe: bool,
}

const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "exit",
        description: "Quit Vulcan",
        // Always exits cleanly; no state to corrupt.
        mid_turn_safe: true,
    },
    SlashCommand {
        name: "quit",
        description: "Quit Vulcan",
        mid_turn_safe: true,
    },
    SlashCommand {
        name: "help",
        description: "Show available commands",
        // Pure UI: pushes a system message.
        mid_turn_safe: true,
    },
    SlashCommand {
        name: "clear",
        description: "Clear message history",
        // Destructive: would nuke the in-flight User+Agent pair the agent
        // loop is streaming into. Defer until idle.
        mid_turn_safe: false,
    },
    SlashCommand {
        name: "view",
        description: "Cycle to next view (or 1-5)",
        mid_turn_safe: true,
    },
    SlashCommand {
        name: "reasoning",
        description: "Toggle reasoning trace",
        mid_turn_safe: true,
    },
    SlashCommand {
        name: "search",
        description: "Search past sessions: /search <query>",
        // Holds agent.lock().await — would deadlock against the in-flight
        // run_prompt_stream task. Defer until idle.
        mid_turn_safe: false,
    },
    SlashCommand {
        name: "model",
        description: "List or switch models: /model [id]",
        // Rebuilds the provider for future turns and may fetch the catalog.
        // Defer until idle so the in-flight provider stream is untouched.
        mid_turn_safe: false,
    },
    SlashCommand {
        name: "provider",
        description: "List or switch named providers: /provider [name|default]",
        // Rebuilds the provider against a different profile; same idle
        // requirement as /model.
        mid_turn_safe: false,
    },
    SlashCommand {
        name: "diff-style",
        description: "Set diff render: /diff-style <unified|side-by-side|inline>",
        mid_turn_safe: true,
    },
    SlashCommand {
        name: "resume",
        description: "Open session picker to switch to another session",
        mid_turn_safe: false,
    },
];

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

#[allow(dead_code)] // retained for tests and potential `--help`-style printers.
fn format_model_list(active_model: &str, models: &[crate::provider::catalog::ModelInfo]) -> String {
    let mut out = format!("Models from active provider ({} total):", models.len());
    for model in models.iter().take(30) {
        let marker = if model.id == active_model { "*" } else { " " };
        let context = if model.context_length > 0 {
            crate::tui::state::format_thousands(model.context_length as u32)
        } else {
            "unknown".into()
        };
        let mut flags = Vec::new();
        if model.features.tools {
            flags.push("tools");
        }
        if model.features.reasoning {
            flags.push("reasoning");
        }
        if model.features.vision {
            flags.push("vision");
        }
        if model.features.json_mode {
            flags.push("json");
        }
        let flags = if flags.is_empty() {
            String::new()
        } else {
            format!(" · {}", flags.join(","))
        };
        out.push_str(&format!("\n  {marker} {} · ctx {context}{flags}", model.id));
    }
    if models.len() > 30 {
        out.push_str(&format!("\n  ... {} more", models.len() - 30));
    }
    out.push_str("\n\nUse /model <id> to switch.");
    out
}

fn build_provider_picker_entries(config: &Config) -> Vec<crate::tui::state::ProviderPickerEntry> {
    use crate::tui::state::ProviderPickerEntry;
    let mut out = Vec::with_capacity(config.providers.len() + 1);
    out.push(ProviderPickerEntry {
        name: None,
        model: config.provider.model.clone(),
        base_url: config.provider.base_url.clone(),
    });
    let mut names: Vec<&String> = config.providers.keys().collect();
    names.sort();
    for name in names {
        let cfg = &config.providers[name];
        out.push(ProviderPickerEntry {
            name: Some(name.clone()),
            model: cfg.model.clone(),
            base_url: cfg.base_url.clone(),
        });
    }
    out
}

#[allow(dead_code)] // retained for tests and potential `--help`-style printers.
fn format_provider_list(config: &Config, active: Option<&str>) -> String {
    let mut out = String::from("Provider profiles:");
    let default_marker = if active.is_none() { "*" } else { " " };
    out.push_str(&format!(
        "\n  {default_marker} default · {} · {}",
        config.provider.base_url, config.provider.model,
    ));

    let mut names: Vec<&String> = config.providers.keys().collect();
    names.sort();
    for name in names {
        let cfg = &config.providers[name];
        let marker = if active == Some(name.as_str()) {
            "*"
        } else {
            " "
        };
        out.push_str(&format!(
            "\n  {marker} {name} · {} · {}",
            cfg.base_url, cfg.model,
        ));
    }
    if config.providers.is_empty() {
        out.push_str("\n  (no named [providers.<name>] profiles configured)");
    }
    out.push_str("\n\nUse /provider <name> to switch, /provider default to revert.");
    out
}

fn filter_commands(prefix: &str) -> Vec<&'static SlashCommand> {
    if prefix.is_empty() {
        return SLASH_COMMANDS.iter().collect();
    }
    let lower = prefix.to_lowercase();
    SLASH_COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(&lower))
        .collect()
}

/// Same matching logic as the palette renderer in the main loop — exposed
/// as a helper so the key handler can decide what to highlight or commit
/// without duplicating prefix logic (YYC-70).
fn current_palette(input: &str) -> Vec<&'static SlashCommand> {
    if input == "/" {
        SLASH_COMMANDS.iter().collect()
    } else if input.starts_with('/') && input.len() > 1 {
        filter_commands(&input[1..])
    } else {
        Vec::new()
    }
}

fn complete_slash(prefix: &str) -> Option<String> {
    let matches = filter_commands(prefix);
    if matches.is_empty() {
        return None;
    }
    if matches.len() == 1 {
        return Some(matches[0].name.to_string());
    }
    let first = matches[0].name.as_bytes();
    let mut common = first.len();
    for m in &matches[1..] {
        let bytes = m.name.as_bytes();
        common = common.min(bytes.len());
        for (i, &b) in first.iter().enumerate().take(common) {
            if b != bytes[i] {
                common = i;
                break;
            }
        }
    }
    if common > prefix.len() {
        Some(matches[0].name[..common].to_string())
    } else {
        None
    }
}

/// Spawn a fresh agent turn for `msg`. Updates chat state (User + empty
/// Agent message), flips thinking on, re-engages auto-follow, then spawns
/// `run_prompt_stream` against the agent. Used by the Enter handler for
/// new submissions and by the Done handler when draining the queue
/// (YYC-61).
fn submit_prompt(
    app: &mut AppState,
    agent: &Arc<Mutex<Agent>>,
    stream_tx: &mpsc::UnboundedSender<StreamEvent>,
    msg: String,
) {
    app.messages.push(ChatMessage {
        role: ChatRole::User,
        content: msg.clone(),
        ..Default::default()
    });
    app.messages.push(ChatMessage {
        role: ChatRole::Agent,
        content: String::new(),
        ..Default::default()
    });
    app.thinking = true;
    app.at_bottom = true;
    app.note_prompt_submitted(&msg);

    let tx = stream_tx.clone();
    let a = agent.clone();
    tokio::spawn(async move {
        let mut a = a.lock().await;
        let _ = a.run_prompt_stream(&msg, tx).await;
    });
}

async fn refresh_sessions(agent: &Arc<Mutex<Agent>>, app: &mut AppState) {
    let (summaries, active_session_id) = {
        let a = agent.lock().await;
        (
            a.memory().list_sessions(12).unwrap_or_default(),
            a.session_id().to_string(),
        )
    };
    app.hydrate_sessions(&summaries, &active_session_id);
}

async fn handle_stream_event(
    app: &mut AppState,
    agent: &Arc<Mutex<Agent>>,
    stream_tx: &mpsc::UnboundedSender<StreamEvent>,
    ev: StreamEvent,
) {
    match ev {
        StreamEvent::Text(chunk) => {
            if let Some(last) = app.messages.last_mut() {
                if matches!(last.role, ChatRole::Agent) {
                    // Append to both the segment timeline (the YYC-71
                    // ordered renderer) and the legacy `content` field
                    // (kept so other code that peeks at .content keeps working).
                    last.append_text(&chunk);
                    last.content.push_str(&chunk);
                }
            }
        }
        StreamEvent::Reasoning(chunk) => {
            // Per-token reasoning trace from thinking-mode models. Push to
            // the segment timeline so it interleaves with tool calls in
            // render order; also append to the legacy `reasoning` field so
            // latest_reasoning() etc. continue to work.
            if let Some(last) = app.messages.last_mut() {
                if matches!(last.role, ChatRole::Agent) {
                    last.append_reasoning(&chunk);
                    last.reasoning.push_str(&chunk);
                }
            }
            app.note_reasoning();
        }
        StreamEvent::Done(resp) => {
            app.thinking = false;
            if let Some(usage) = resp.usage {
                // YYC-60: track lifetime totals for cost (YYC-67) and the
                // latest prompt size for the in-status capacity bar.
                app.prompt_tokens_total = app
                    .prompt_tokens_total
                    .saturating_add(usage.prompt_tokens as u32);
                app.completion_tokens_total = app
                    .completion_tokens_total
                    .saturating_add(usage.completion_tokens as u32);
                app.prompt_tokens_last = usage.prompt_tokens as u32;
            }
            app.note_done();
            refresh_sessions(agent, app).await;
            // YYC-61: drain one queued prompt per turn end. Subsequent queued
            // prompts ride the next Done event in the same way.
            if let Some(next) = app.queue.pop_front() {
                submit_prompt(app, agent, stream_tx, next);
            }
        }
        StreamEvent::Error(e) => {
            if let Some(last) = app.messages.last_mut() {
                if last.content.is_empty() {
                    last.set_content(format!("⚠ Error: {e}"));
                }
            }
            app.thinking = false;
            // YYC-67: record provider-level error for telemetry.
            app.provider_errors_total = app.provider_errors_total.saturating_add(1);
            app.note_error(&e);
        }
        StreamEvent::ToolCallStart {
            name, args_summary, ..
        } => {
            // YYC-71: push the tool-call segment into the timeline
            // (interleaved with reasoning/text). YYC-74: carry the args
            // summary so the card has structured context.
            if let Some(last) = app.messages.last_mut() {
                if matches!(last.role, ChatRole::Agent) {
                    last.push_tool_start_with(name.clone(), args_summary);
                }
            }
            app.note_tool_start(&name);
        }
        StreamEvent::ToolCallEnd {
            name,
            ok,
            output_preview,
            result_meta,
            elided_lines,
            elapsed_ms,
            ..
        } => {
            if let Some(last) = app.messages.last_mut() {
                if matches!(last.role, ChatRole::Agent) {
                    // YYC-74: stamp preview + meta + timing onto the matching
                    // segment for the card. YYC-78: stash elided count for the
                    // collapse footer.
                    last.finish_tool_with(
                        &name,
                        ok,
                        output_preview,
                        result_meta,
                        elided_lines,
                        Some(elapsed_ms),
                    );
                }
            }
            // YYC-67: tool call telemetry.
            app.tool_calls_total = app.tool_calls_total.saturating_add(1);
            if !ok {
                app.tool_errors_total = app.tool_errors_total.saturating_add(1);
            }
            app.note_tool_end(&name, ok);
        }
    }
}

pub async fn run_tui(config: &Config, resume: ResumeTarget) -> Result<()> {
    let mut terminal = init_terminal()?;

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

    // streaming
    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel::<StreamEvent>();

    // ── Hook registry: audit-log + (room for safety-gate, etc.). Built-in
    // hooks (skills) are registered by Agent::with_hooks itself.
    let mut hook_reg = HookRegistry::new();
    let (audit_hook, audit_buf) = AuditHook::new(200);
    hook_reg.register(audit_hook);

    // ── AgentPause channel: when SafetyHook (or future hooks) needs user
    // input mid-loop, it sends an AgentPause; the main TUI loop renders an
    // overlay and routes the response back via the pause's oneshot reply.
    let (pause_tx, mut pause_rx) = pause::channel(8);
    let pause_tx_for_agent = pause_tx.clone();

    // ── Long-lived agent: one per TUI session, shared across prompts so
    // hook handlers' state (audit log, rate limits, etc.) survives turns.
    let agent = Arc::new(Mutex::new(
        Agent::with_hooks_and_pause(config, hook_reg, Some(pause_tx_for_agent)).await?,
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
    }
    refresh_sessions(&agent, &mut app).await;

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
            Some(filter_commands(&app.input[1..]))
        } else if app.input == "/" {
            Some(SLASH_COMMANDS.iter().collect())
        } else {
            None
        };

        // YYC-58: derive prompt mode from current state once per tick.
        app.refresh_prompt_mode();

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
                draw_palette(f, area, pal, app.slash_menu_selection, &app.theme);
            }

            // YYC-86: session picker overlay (--resume flag).
            if app.show_session_picker {
                draw_session_picker(f, area, &app);
            }
            // YYC-97: model / provider picker overlays.
            if app.show_model_picker {
                draw_model_picker(f, area, &app);
            }
            if app.show_provider_picker {
                draw_provider_picker(f, area, &app);
            }
            // YYC-75: diff scrubber overlay.
            if app.show_diff_scrubber {
                draw_diff_scrubber(f, area, &app);
            }
        })?;
        last_draw = Instant::now();

        // ── Diff scrubber overlay (YYC-75): intercept input until resolved.
        if app.show_diff_scrubber {
            tokio::select! {
                ev = key_rx.recv() => {
                    match ev {
                        Some(KeyEv::Press(event)) => {
                            if let Event::Key(key) = event {
                                if key.kind == KeyEventKind::Press {
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
                            if let Event::Key(key) = event {
                                if key.kind == KeyEventKind::Press {
                                    let mut commit_id: Option<String> = None;
                                    let mut close = false;
                                    match key.code {
                                        KeyCode::Up | KeyCode::Char('k') => {
                                            picker_move(&mut app, -1);
                                        }
                                        KeyCode::Down | KeyCode::Char('j') => {
                                            picker_move(&mut app, 1);
                                        }
                                        KeyCode::Left | KeyCode::Char('h') => {
                                            if app.model_picker_focus == 0 {
                                                close = true;
                                            } else {
                                                app.model_picker_focus -= 1;
                                                app.model_picker_path
                                                    .truncate(app.model_picker_focus + 1);
                                            }
                                        }
                                        KeyCode::Right | KeyCode::Char('l') => {
                                            if let Some(id) = picker_drill_or_commit(&mut app)
                                            {
                                                commit_id = Some(id);
                                            }
                                        }
                                        KeyCode::Enter => {
                                            if let Some(id) = picker_commit_current(&app) {
                                                commit_id = Some(id);
                                            }
                                        }
                                        KeyCode::Esc | KeyCode::Char('q') => close = true,
                                        _ => {}
                                    }
                                    if let Some(id) = commit_id {
                                        let result = {
                                            let mut a = agent.lock().await;
                                            a.switch_model(&id).await
                                        };
                                        match result {
                                            Ok(selection) => {
                                                app.model_label =
                                                    selection.model.id.clone();
                                                app.token_max =
                                                    selection.max_context as u32;
                                                app.pricing = selection.pricing;
                                                app.messages.push(ChatMessage {
                                                    role: ChatRole::System,
                                                    content: format!(
                                                        "Model switched to {} · context {}",
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
        if app.show_provider_picker {
            tokio::select! {
                ev = key_rx.recv() => {
                    match ev {
                        Some(KeyEv::Press(event)) => {
                            if let Event::Key(key) = event {
                                if key.kind == KeyEventKind::Press {
                                    match key.code {
                                        KeyCode::Up | KeyCode::Char('k') => {
                                            app.provider_picker_selection = app.provider_picker_selection.saturating_sub(1);
                                        }
                                        KeyCode::Down | KeyCode::Char('j') => {
                                            let max = app.provider_picker_items.len().saturating_sub(1);
                                            app.provider_picker_selection = app.provider_picker_selection.saturating_add(1).min(max);
                                        }
                                        KeyCode::Enter => {
                                            let idx = app.provider_picker_selection.min(app.provider_picker_items.len().saturating_sub(1));
                                            if let Some(picked) = app.provider_picker_items.get(idx).cloned() {
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
                                            app.show_provider_picker = false;
                                        }
                                        KeyCode::Esc => {
                                            app.show_provider_picker = false;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        Some(KeyEv::Error(e)) => {
                            tracing::error!("Terminal input error (provider picker): {e}");
                            app.show_provider_picker = false;
                        }
                        None => app.show_provider_picker = false,
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
                            if let Event::Key(key) = event {
                                if key.kind == KeyEventKind::Press {
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
                                                refresh_sessions(&agent, &mut app).await;
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
                        if let Event::Key(key) = event {
                            if key.kind == KeyEventKind::Press {
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
                                    refresh_sessions(&agent, &mut app).await;
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
                                                let candidates = current_palette(&app.input);
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
                                            if pending_quit {
                                                exit = true;
                                            } else if app.thinking {
                                                // Turn in flight: single Ctrl+C cancels it.
                                                // Double Ctrl+C still exits (pending_quit path).
                                                let a = agent.clone();
                                                tokio::spawn(async move {
                                                    a.lock().await.cancel_current_turn();
                                                });
                                                app.messages.push(ChatMessage {
                                                    role: ChatRole::System,
                                                    content: "Cancelling current turn… (Ctrl+C again to quit)".into(),
                                                    reasoning: String::new(),
                            segments: Vec::new(),
                                                    render_version: 0,
                                                });
                                                pending_quit = true;
                                            } else {
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
                                        _ => {}
                                    }
                                }

                                match key.code {
                                    KeyCode::Enter => {
                                        // YYC-70: when the slash menu is open with at least one
                                        // match, Enter commits the highlighted command.
                                        if app.input.starts_with('/') {
                                            let candidates = current_palette(&app.input);
                                            if !candidates.is_empty() {
                                                let idx = app.slash_menu_selection.min(candidates.len() - 1);
                                                app.input = format!("/{}", candidates[idx].name);
                                            }
                                        }
                                        if app.input.is_empty() {
                                            continue;
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
                                            SLASH_COMMANDS
                                                .iter()
                                                .find(|c| c.name == head)
                                                .map(|c| c.mid_turn_safe)
                                                .unwrap_or(false)
                                        } else {
                                            false
                                        };
                                        if app.thinking && is_slash && !mid_turn_safe {
                                            let cmd_text = app.input.trim().to_string();
                                            app.input.clear();
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
                                        // YYC-61: plain text during busy → queue.
                                        if !app.input.is_empty()
                                            && app.thinking
                                            && !is_slash
                                        {
                                            let msg = app.input.trim().to_string();
                                            if !msg.is_empty() {
                                                app.queue.push_back(msg);
                                            }
                                            app.input.clear();
                                            app.slash_menu_selection = 0;
                                            pending_quit = false;
                                            continue;
                                        }
                                        // Idle, or busy + mid-turn-safe slash command — fall
                                        // through to dispatch.
                                        if !app.input.is_empty() {
                                            let msg = app.input.trim().to_string();
                                            app.input.clear();
                                            app.slash_menu_selection = 0;
                                            pending_quit = false;

                                            // slash commands
                                            if msg.starts_with('/') {
                                                let body = &msg[1..];
                                                match body {
                                                    "exit" | "quit" => { exit = true; continue; }
                                                    "help" => {
                                                        let mut help = String::from("Commands:");
                                                        for cmd in SLASH_COMMANDS {
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
                                                        if let Ok(n) = s[5..].trim().parse::<u8>() {
                                                            if let Some(v) = View::from_index(n) {
                                                                app.view = v;
                                                            }
                                                        }
                                                        continue;
                                                    }
                                                    "resume" => {
                                                        refresh_sessions(&agent, &mut app).await;
                                                        app.show_session_picker = true;
                                                        app.session_picker_selection = 0;
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
                                                            app.provider_picker_items =
                                                                build_provider_picker_entries(config);
                                                            let active_idx = app
                                                                .provider_picker_items
                                                                .iter()
                                                                .position(|e| e.name == active)
                                                                .unwrap_or(0);
                                                            app.provider_picker_selection = active_idx;
                                                            app.show_provider_picker = true;
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
                                                                Ok(models) => {
                                                                    let provider_label = {
                                                                        let a = agent.lock().await;
                                                                        a.active_profile()
                                                                            .map(str::to_string)
                                                                            .unwrap_or_else(|| "default".into())
                                                                    };
                                                                    let tree =
                                                                        crate::tui::model_picker::build_model_tree(
                                                                            &provider_label,
                                                                            &models,
                                                                        );
                                                                    app.model_picker_path =
                                                                        initial_path_for_active_model(
                                                                            &tree,
                                                                            &active,
                                                                            &models,
                                                                        );
                                                                    app.model_picker_focus = app
                                                                        .model_picker_path
                                                                        .len()
                                                                        .saturating_sub(1);
                                                                    app.model_picker_tree = tree;
                                                                    app.model_picker_items = models;
                                                                    app.show_model_picker = true;
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

                                            submit_prompt(&mut app, &agent, &stream_tx, msg);
                                        }
                                    }
                                    KeyCode::Char(c) => {
                                        pending_quit = false;
                                        app.input.push(c);
                                        // Re-filtering may shrink the menu; keep the highlight
                                        // anchored at the top so the visible top row is selected.
                                        app.slash_menu_selection = 0;
                                    }
                                    KeyCode::Backspace => {
                                        pending_quit = false;
                                        app.input.pop();
                                        app.slash_menu_selection = 0;
                                    }
                                    KeyCode::Tab => {
                                        pending_quit = false;
                                        if let Some(rest) = app.input.strip_prefix('/') {
                                            if let Some(c) = complete_slash(rest) {
                                                app.input = format!("/{c}");
                                            }
                                        }
                                    }
                                    KeyCode::Up => {
                                        // YYC-70: arrows navigate the slash menu when open.
                                        if app.input.starts_with('/') {
                                            let candidates = current_palette(&app.input);
                                            if !candidates.is_empty() {
                                                app.slash_menu_selection =
                                                    app.slash_menu_selection.saturating_sub(1);
                                                continue;
                                            }
                                        }
                                        app.scroll = app.scroll.saturating_sub(1);
                                        app.at_bottom = false;
                                    }
                                    KeyCode::Down => {
                                        if app.input.starts_with('/') {
                                            let candidates = current_palette(&app.input);
                                            if !candidates.is_empty() {
                                                let len = candidates.len();
                                                app.slash_menu_selection =
                                                    (app.slash_menu_selection + 1).min(len - 1);
                                                continue;
                                            }
                                        }
                                        let max = app.chat_max_scroll.get();
                                        app.scroll = app.scroll.saturating_add(1).min(max);
                                        app.at_bottom = app.scroll >= max;
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
                                        if app.input.starts_with('/') {
                                            app.input.clear();
                                            app.slash_menu_selection = 0;
                                        } else {
                                            exit = true;
                                        }
                                    }
                                    _ => { pending_quit = false; }
                                }
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
                        let mut force_redraw = stream_event_forces_redraw(&ev);
                        handle_stream_event(&mut app, &agent, &stream_tx, ev).await;
                        while let Ok(ev) = stream_rx.try_recv() {
                            force_redraw |= stream_event_forces_redraw(&ev);
                            handle_stream_event(&mut app, &agent, &stream_tx, ev).await;
                        }
                        if !force_redraw {
                            if let RenderWake::Wait(delay) =
                                render_wake_for_stream_batch(last_draw, Instant::now(), false)
                            {
                                tokio::time::sleep(delay).await;
                            }
                        }
                    }
                    None => app.thinking = false,
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

    restore_terminal()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_batching_caps_stream_redraws_to_frame_budget() {
        let start = Instant::now();

        assert_eq!(
            render_wake_for_stream_batch(start, start + Duration::from_millis(1), false),
            RenderWake::Wait(Duration::from_millis(15))
        );
    }

    #[test]
    fn input_events_render_immediately() {
        let start = Instant::now();

        assert_eq!(
            render_wake_for_stream_batch(start, start + Duration::from_millis(1), true),
            RenderWake::Now
        );
    }

    #[test]
    fn model_command_is_available_and_deferred_mid_turn() {
        let command = SLASH_COMMANDS
            .iter()
            .find(|cmd| cmd.name == "model")
            .expect("model slash command");

        assert!(!command.mid_turn_safe);
        assert_eq!(filter_commands("mod")[0].name, "model");
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

        let entries = build_provider_picker_entries(&config);
        assert_eq!(entries.len(), 3);
        assert!(entries[0].name.is_none());
        assert_eq!(entries[0].model, "deepseek/v4");
        assert_eq!(entries[1].name.as_deref(), Some("alpha"));
        assert_eq!(entries[2].name.as_deref(), Some("local"));
    }

    #[test]
    fn provider_command_is_available_and_deferred_mid_turn() {
        let command = SLASH_COMMANDS
            .iter()
            .find(|cmd| cmd.name == "provider")
            .expect("provider slash command");

        assert!(!command.mid_turn_safe);
        assert_eq!(filter_commands("prov")[0].name, "provider");
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

        let active_default = format_provider_list(&config, None);
        assert!(active_default.contains("* default · https://openrouter.ai/api/v1 · deepseek/v4"));
        assert!(active_default.contains("  local · http://localhost:11434/v1 · qwen2.5"));

        let active_local = format_provider_list(&config, Some("local"));
        assert!(active_local.contains("  default · https://openrouter.ai/api/v1"));
        assert!(active_local.contains("* local · http://localhost:11434/v1"));
    }

    #[test]
    fn format_provider_list_handles_no_named_profiles() {
        use crate::config::Config;
        let config = Config::default();
        let report = format_provider_list(&config, None);
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

        let report = format_model_list("model-a", &models);

        assert!(report.contains("* model-a · ctx 1,000 · tools,json"));
        assert!(report.contains("  model-b · ctx unknown"));
        assert!(report.contains("Use /model <id> to switch."));
    }
}

fn draw_palette(
    f: &mut ratatui::Frame,
    area: Rect,
    cmds: &[&SlashCommand],
    selected: usize,
    theme: &Theme,
) {
    if area.height == 0 {
        return;
    }
    // Title bar
    let bar = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    let mut header_text = " ▓▓ COMMANDS".to_string();
    if (header_text.chars().count() as u16) < bar.width {
        header_text.push_str(&" ".repeat(bar.width as usize - header_text.chars().count()));
    }
    f.render_widget(
        Paragraph::new(header_text).style(theme.accent.add_modifier(Modifier::BOLD)),
        bar,
    );
    let inner = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height.saturating_sub(1),
    };
    let mut lines = Vec::new();
    if cmds.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No matching commands",
            theme.muted,
        )));
    } else {
        let active = selected.min(cmds.len().saturating_sub(1));
        for (i, cmd) in cmds.iter().enumerate() {
            let is_active = i == active;
            // YYC-70: highlight the active row by swapping fg/bg of accent
            // (gives a visible selection bar regardless of active theme).
            let (prefix, name_style, desc_style) = if is_active {
                let active_style = theme.accent.add_modifier(Modifier::BOLD);
                ("▸ ", active_style, active_style)
            } else {
                (
                    "  ",
                    theme.accent.add_modifier(Modifier::BOLD),
                    theme.assistant,
                )
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{prefix}/{:<12}", cmd.name), name_style),
                Span::styled(cmd.description, desc_style),
            ]));
        }
    }
    f.render_widget(Paragraph::new(lines).style(body()), inner);
}

fn draw_session_picker(f: &mut ratatui::Frame, area: Rect, app: &AppState) {
    let theme = &app.theme;
    let width = area.width.min(56);
    let height = (app.sessions.len() as u16 + 6).min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;

    let box_area = Rect {
        x,
        y,
        width,
        height,
    };
    if box_area.height < 4 {
        return;
    }

    // Title bar
    let bar = Rect {
        x: box_area.x,
        y: box_area.y,
        width: box_area.width,
        height: 1,
    };
    let mut title = "  Resume a Session  ".to_string();
    if (title.chars().count() as u16) < bar.width {
        let pad = bar.width as usize - title.chars().count();
        title = format!(
            "{}{}{}",
            " ".repeat(pad / 2),
            title.trim(),
            " ".repeat(pad - pad / 2)
        );
    }
    f.render_widget(
        Paragraph::new(title).style(theme.accent.add_modifier(Modifier::BOLD)),
        bar,
    );

    // Session list
    let list_area = Rect {
        x: box_area.x,
        y: box_area.y + 1,
        width: box_area.width,
        height: box_area.height.saturating_sub(2),
    };
    let mut lines: Vec<Line<'static>> = Vec::new();

    if app.sessions.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No saved sessions found.",
            theme.muted,
        )));
        lines.push(Line::from(Span::styled(
            "  Start a conversation and sessions will appear here.",
            theme.muted,
        )));
    } else {
        let active = app
            .session_picker_selection
            .min(app.sessions.len().saturating_sub(1));
        for (i, s) in app.sessions.iter().enumerate() {
            let is_active = i == active;
            let marker = if is_active { "▸ " } else { "  " };
            let status_style = match s.status {
                SessionStatus::Live => theme.success,
                SessionStatus::Saved => theme.system,
            };
            let status_label = match s.status {
                SessionStatus::Live => "LIVE",
                SessionStatus::Saved => "saved",
            };

            let dt = chrono::DateTime::from_timestamp(s.last_active, 0)
                .map(|d| {
                    d.with_timezone(&chrono::Local)
                        .format("%b %d %H:%M")
                        .to_string()
                })
                .unwrap_or_default();

            let name_style = Style::default()
                .fg(theme.body_fg)
                .add_modifier(if is_active {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                });

            lines.push(Line::from(vec![
                Span::styled(marker, name_style.add_modifier(Modifier::BOLD)),
                Span::styled("█ ", status_style),
                Span::styled(format!("{:<12}", short_id(&s.id)), name_style),
                Span::styled(format!("{:>4}m", s.message_count), theme.muted),
                Span::styled(format!("  {} ", dt), theme.muted),
                Span::styled(status_label, status_style.add_modifier(Modifier::BOLD)),
            ]));

            if let Some(preview) = &s.preview {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("  {}", preview),
                        theme.muted.add_modifier(Modifier::DIM),
                    ),
                ]));
            }
        }
    }

    // Footer hint
    let hint = "  ↑↓ navigate · Enter select · Esc cancel  ";
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(hint, theme.muted)));

    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.body_bg)),
        list_area,
    );

    // Draw a border around the whole thing
    draw_picker_border(f, box_area, theme);
}

fn draw_model_picker(f: &mut ratatui::Frame, area: Rect, app: &AppState) {
    let theme = &app.theme;
    let width = area.width.min(72);
    let rows = (app.model_picker_items.len() as u16).min(20);
    let height = (rows + 5).min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let box_area = Rect {
        x,
        y,
        width,
        height,
    };
    if box_area.height < 4 {
        return;
    }

    let bar = Rect {
        x: box_area.x,
        y: box_area.y,
        width: box_area.width,
        height: 1,
    };
    let mut title = "  Switch Model  ".to_string();
    if (title.chars().count() as u16) < bar.width {
        let pad = bar.width as usize - title.chars().count();
        title = format!(
            "{}{}{}",
            " ".repeat(pad / 2),
            title.trim(),
            " ".repeat(pad - pad / 2)
        );
    }
    f.render_widget(
        Paragraph::new(title).style(theme.accent.add_modifier(Modifier::BOLD)),
        bar,
    );

    let list_area = Rect {
        x: box_area.x,
        y: box_area.y + 1,
        width: box_area.width,
        height: box_area.height.saturating_sub(2),
    };

    if app.model_picker_items.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  No models in provider catalog.",
                theme.muted,
            ))),
            list_area,
        );
        draw_picker_border(f, box_area, theme);
        return;
    }

    // Mini-files-style miller columns. The number of visible columns is
    // capped to whatever the box width can fit (each column is roughly
    // 20–28 chars). Always reserve the rightmost column for details.
    let drilled_depth = app.model_picker_path.len();
    let max_tree_depth = app.model_picker_tree.max_depth();
    let total_columns = max_tree_depth.max(1) + 1; // +1 for details

    let col_width = (list_area.width / total_columns as u16).max(16);
    let cols: Vec<Rect> = (0..total_columns)
        .map(|i| Rect {
            x: list_area.x + i as u16 * col_width,
            y: list_area.y,
            width: col_width,
            height: list_area.height,
        })
        .collect();

    // Render each tree column.
    for col_idx in 0..max_tree_depth {
        let path_prefix: Vec<usize> = app
            .model_picker_path
            .iter()
            .copied()
            .take(col_idx)
            .collect();
        let nodes = app.model_picker_tree.column_at(col_idx, &path_prefix);
        let selection = app
            .model_picker_path
            .get(col_idx)
            .copied()
            .unwrap_or(0)
            .min(nodes.len().saturating_sub(1));
        let is_focused = col_idx == app.model_picker_focus;
        render_picker_column(f, cols[col_idx], nodes, selection, is_focused, theme);
    }

    // Details panel at the rightmost column.
    let details_col = cols.last().copied().unwrap_or(list_area);
    let detail_lines = build_picker_details(app);
    f.render_widget(
        Paragraph::new(detail_lines).wrap(Wrap { trim: false }),
        details_col,
    );

    let hint = "  hjkl move · Enter select · Esc cancel  (drilled: column ";
    let footer_line = format!(
        "{hint}{}/{})",
        app.model_picker_focus + 1,
        max_tree_depth.max(1)
    );
    let footer_rect = Rect {
        x: list_area.x,
        y: list_area.y + list_area.height.saturating_sub(1),
        width: list_area.width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(footer_line, theme.muted))),
        footer_rect,
    );
    let _ = drilled_depth; // reserved for future filter UI

    draw_picker_border(f, box_area, theme);
}

fn render_picker_column(
    f: &mut ratatui::Frame,
    area: Rect,
    nodes: &[crate::tui::model_picker::TreeNode],
    selection: usize,
    is_focused: bool,
    theme: &Theme,
) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if nodes.is_empty() {
        lines.push(Line::from(Span::styled("  ·", theme.muted)));
    } else {
        let visible = area.height.saturating_sub(2) as usize;
        let start = selection.saturating_sub(visible.saturating_sub(1) / 2);
        let end = (start + visible).min(nodes.len());
        for (i, node) in nodes.iter().enumerate().take(end).skip(start) {
            let is_active = i == selection;
            let marker = if is_active && is_focused {
                "▸ "
            } else if is_active {
                "│ "
            } else {
                "  "
            };
            let mut style = Style::default();
            if is_active {
                style = style.add_modifier(Modifier::BOLD);
                if is_focused {
                    style = if let Some(fg) = theme.accent.fg {
                        style.fg(fg)
                    } else {
                        style.add_modifier(Modifier::REVERSED)
                    };
                }
            }
            let suffix = if node.children.is_empty() && node.model_index.is_some() {
                ""
            } else {
                "›"
            };
            let label = trim_to_width(&node.label, area.width.saturating_sub(4) as usize);
            lines.push(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(label, style),
                Span::styled(format!(" {suffix}"), theme.muted),
            ]));
        }
        if start > 0 || end < nodes.len() {
            lines.push(Line::from(Span::styled(
                format!("  …{}/{}", end, nodes.len()),
                theme.muted.add_modifier(Modifier::DIM),
            )));
        }
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn build_picker_details(app: &AppState) -> Vec<Line<'static>> {
    let theme = &app.theme;
    let mut lines = Vec::new();
    let leaf_idx = picker_current_leaf(&app.model_picker_tree, &app.model_picker_path);
    let Some(idx) = leaf_idx else {
        lines.push(Line::from(Span::styled(
            "  drill in (l/→) for details",
            theme.muted,
        )));
        return lines;
    };
    let Some(model) = app.model_picker_items.get(idx) else {
        return lines;
    };
    lines.push(Line::from(Span::styled(
        format!(" {}", model.id),
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
    let ctx = if model.context_length > 0 {
        crate::tui::state::format_thousands(model.context_length as u32)
    } else {
        "?".into()
    };
    lines.push(Line::from(Span::styled(
        format!(" context  : {ctx}"),
        theme.muted,
    )));
    let mut flags = Vec::new();
    if model.features.tools {
        flags.push("tools");
    }
    if model.features.reasoning {
        flags.push("reasoning");
    }
    if model.features.vision {
        flags.push("vision");
    }
    if model.features.json_mode {
        flags.push("json");
    }
    let flag_str = if flags.is_empty() {
        "(none reported)".to_string()
    } else {
        flags.join(", ")
    };
    lines.push(Line::from(Span::styled(
        format!(" features : {flag_str}"),
        theme.muted,
    )));
    if let Some(p) = &model.pricing {
        lines.push(Line::from(Span::styled(
            format!(
                " pricing  : ${:.4}/1k in · ${:.4}/1k out",
                p.input_per_token * 1000.0,
                p.output_per_token * 1000.0,
            ),
            theme.muted,
        )));
    }
    if let Some(top) = &model.top_provider {
        lines.push(Line::from(Span::styled(
            format!(" upstream : {top}"),
            theme.muted,
        )));
    }
    lines
}

fn trim_to_width(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= width {
        return s.to_string();
    }
    let head: String = chars.iter().take(width.saturating_sub(1)).collect();
    format!("{head}…")
}

fn picker_move(app: &mut AppState, delta: i32) {
    let depth = app.model_picker_focus;
    let path_prefix: Vec<usize> = app.model_picker_path.iter().copied().take(depth).collect();
    let len = app.model_picker_tree.column_at(depth, &path_prefix).len();
    if len == 0 {
        return;
    }
    while app.model_picker_path.len() <= depth {
        app.model_picker_path.push(0);
    }
    let cur = app.model_picker_path[depth] as i32 + delta;
    let max = (len - 1) as i32;
    app.model_picker_path[depth] = cur.clamp(0, max) as usize;
    // Reset deeper selections — the active branch changed.
    app.model_picker_path.truncate(depth + 1);
}

fn picker_drill_or_commit(app: &mut AppState) -> Option<String> {
    let depth = app.model_picker_focus;
    let path_prefix: Vec<usize> = app.model_picker_path.iter().copied().take(depth).collect();
    let nodes = app
        .model_picker_tree
        .column_at(depth, &path_prefix)
        .to_vec();
    let sel = app.model_picker_path.get(depth).copied().unwrap_or(0);
    let Some(node) = nodes.get(sel) else {
        return None;
    };
    if node.children.is_empty() {
        // Leaf — commit.
        return node
            .model_index
            .and_then(|i| app.model_picker_items.get(i))
            .map(|m| m.id.clone());
    }
    // Drill: focus next column, default selection 0.
    while app.model_picker_path.len() <= depth + 1 {
        app.model_picker_path.push(0);
    }
    app.model_picker_path[depth + 1] = 0;
    app.model_picker_focus = depth + 1;
    None
}

fn picker_commit_current(app: &AppState) -> Option<String> {
    picker_current_leaf(&app.model_picker_tree, &app.model_picker_path)
        .and_then(|i| app.model_picker_items.get(i))
        .map(|m| m.id.clone())
}

fn picker_current_leaf(
    tree: &crate::tui::model_picker::ModelTree,
    path: &[usize],
) -> Option<usize> {
    let mut current: &[crate::tui::model_picker::TreeNode] = &tree.labs;
    let mut leaf: Option<usize> = None;
    for &idx in path {
        let node = current.get(idx)?;
        if node.children.is_empty() {
            return node.model_index;
        }
        leaf = node.model_index;
        current = &node.children;
    }
    // Path didn't reach a leaf — return last seen leaf marker (None for
    // internal-only nodes).
    leaf
}

fn initial_path_for_active_model(
    tree: &crate::tui::model_picker::ModelTree,
    active_id: &str,
    items: &[crate::provider::catalog::ModelInfo],
) -> Vec<usize> {
    let target = items.iter().position(|m| m.id == active_id);
    fn find_path(
        nodes: &[crate::tui::model_picker::TreeNode],
        target: Option<usize>,
        path: &mut Vec<usize>,
    ) -> bool {
        for (i, node) in nodes.iter().enumerate() {
            path.push(i);
            if node.model_index.is_some() && node.model_index == target {
                return true;
            }
            if find_path(&node.children, target, path) {
                return true;
            }
            path.pop();
        }
        false
    }
    let mut path = Vec::new();
    if !find_path(&tree.labs, target, &mut path) {
        // No exact match — start from column 0 with no drilled selection.
        path.clear();
        path.push(0);
    }
    path
}

fn draw_provider_picker(f: &mut ratatui::Frame, area: Rect, app: &AppState) {
    let theme = &app.theme;
    let width = area.width.min(72);
    let rows = (app.provider_picker_items.len() as u16).min(12);
    let height = (rows + 5).min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let box_area = Rect {
        x,
        y,
        width,
        height,
    };
    if box_area.height < 4 {
        return;
    }

    let bar = Rect {
        x: box_area.x,
        y: box_area.y,
        width: box_area.width,
        height: 1,
    };
    let mut title = "  Switch Provider  ".to_string();
    if (title.chars().count() as u16) < bar.width {
        let pad = bar.width as usize - title.chars().count();
        title = format!(
            "{}{}{}",
            " ".repeat(pad / 2),
            title.trim(),
            " ".repeat(pad - pad / 2)
        );
    }
    f.render_widget(
        Paragraph::new(title).style(theme.accent.add_modifier(Modifier::BOLD)),
        bar,
    );

    let list_area = Rect {
        x: box_area.x,
        y: box_area.y + 1,
        width: box_area.width,
        height: box_area.height.saturating_sub(2),
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.provider_picker_items.is_empty() {
        lines.push(Line::from(Span::styled("  (no providers)", theme.muted)));
    } else {
        let active = app
            .provider_picker_selection
            .min(app.provider_picker_items.len().saturating_sub(1));
        for (i, e) in app.provider_picker_items.iter().enumerate() {
            let is_active = i == active;
            let marker = if is_active { "▸ " } else { "  " };
            let label = e.name.clone().unwrap_or_else(|| "default".into());
            let row_style = Style::default()
                .fg(theme.body_fg)
                .add_modifier(if is_active {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                });
            lines.push(Line::from(vec![
                Span::styled(marker, row_style.add_modifier(Modifier::BOLD)),
                Span::styled(format!("{label:<12}"), row_style),
                Span::styled(format!(" {}", e.model), theme.muted),
                Span::styled(format!("  ({})", e.base_url), theme.muted),
            ]));
        }
    }

    let hint = "  ↑↓ navigate · Enter select · Esc cancel  ";
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(hint, theme.muted)));

    f.render_widget(Paragraph::new(lines), list_area);
    draw_picker_border(f, box_area, theme);
}

fn draw_diff_scrubber(f: &mut ratatui::Frame, area: Rect, app: &AppState) {
    let theme = &app.theme;
    let width = area.width.min(96);
    let total = app.scrubber_hunks.len() as u16;
    let height = (total * 4 + 8).min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let box_area = Rect {
        x,
        y,
        width,
        height,
    };
    if box_area.height < 6 {
        return;
    }

    let bar = Rect {
        x: box_area.x,
        y: box_area.y,
        width: box_area.width,
        height: 1,
    };
    let title = format!(
        "  Edit Scrubber — {} ({} hunks)  ",
        app.scrubber_path, total
    );
    f.render_widget(
        Paragraph::new(title).style(theme.accent.add_modifier(Modifier::BOLD)),
        bar,
    );

    let list_area = Rect {
        x: box_area.x,
        y: box_area.y + 1,
        width: box_area.width,
        height: box_area.height.saturating_sub(2),
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    let active = app
        .scrubber_selection
        .min(app.scrubber_hunks.len().saturating_sub(1));
    for (i, hunk) in app.scrubber_hunks.iter().enumerate() {
        let is_active = i == active;
        let accepted = app.scrubber_accepted.get(i).copied().unwrap_or(true);
        let marker = if is_active { "▸ " } else { "  " };
        let state = if accepted { "[✓]" } else { "[ ]" };
        let header = format!(
            "{marker}{state} hunk {} of {} · line {}",
            i + 1,
            total,
            hunk.line_no
        );
        let header_style = Style::default()
            .fg(theme.body_fg)
            .add_modifier(if is_active {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        lines.push(Line::from(Span::styled(header, header_style)));
        for before in &hunk.before_lines {
            lines.push(Line::from(vec![
                Span::styled(
                    "    - ",
                    Style::default().fg(crate::tui::theme::Palette::RED),
                ),
                Span::styled(
                    before.clone(),
                    Style::default().fg(crate::tui::theme::Palette::RED),
                ),
            ]));
        }
        for after in &hunk.after_lines {
            lines.push(Line::from(vec![
                Span::styled(
                    "    + ",
                    Style::default().fg(crate::tui::theme::Palette::GREEN),
                ),
                Span::styled(
                    after.clone(),
                    Style::default().fg(crate::tui::theme::Palette::GREEN),
                ),
            ]));
        }
        lines.push(Line::from(""));
    }

    let hint = "  ↑↓ navigate · y/n toggle · Y all · N none · Enter apply · Esc cancel  ";
    lines.push(Line::from(Span::styled(hint, theme.muted)));

    f.render_widget(Paragraph::new(lines), list_area);
    draw_picker_border(f, box_area, theme);
}

/// Simple border drawn with box-drawing characters.
fn draw_picker_border(f: &mut ratatui::Frame, r: Rect, theme: &Theme) {
    let style = theme.border;
    // Top
    if r.height > 0 {
        let top = "─".repeat(r.width as usize);
        f.render_widget(
            Paragraph::new(top).style(style),
            Rect {
                x: r.x,
                y: r.y,
                width: r.width,
                height: 1,
            },
        );
    }
    // Bottom
    if r.height > 1 {
        let bot = "─".repeat(r.width as usize);
        f.render_widget(
            Paragraph::new(bot).style(style),
            Rect {
                x: r.x,
                y: r.y + r.height - 1,
                width: r.width,
                height: 1,
            },
        );
    }
    // Left edge (corners overlap — good enough for a 1px line)
    if r.height > 2 {
        let left: Vec<Line<'static>> = (1..r.height - 1)
            .map(|_| Line::from(Span::styled("│", style)))
            .collect();
        f.render_widget(
            Paragraph::new(left),
            Rect {
                x: r.x,
                y: r.y + 1,
                width: 1,
                height: r.height - 2,
            },
        );
        let right: Vec<Line<'static>> = (1..r.height - 1)
            .map(|_| Line::from(Span::styled("│", style)))
            .collect();
        f.render_widget(
            Paragraph::new(right),
            Rect {
                x: r.x + r.width - 1,
                y: r.y + 1,
                width: 1,
                height: r.height - 2,
            },
        );
    }
}

fn init_terminal() -> Result<Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>> {
    ratatui::crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    ratatui::crossterm::execute!(stdout, ratatui::crossterm::terminal::EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal() -> Result<()> {
    let _ = ratatui::crossterm::terminal::disable_raw_mode();
    ratatui::crossterm::execute!(
        std::io::stdout(),
        ratatui::crossterm::terminal::LeaveAlternateScreen,
    )?;
    Ok(())
}
