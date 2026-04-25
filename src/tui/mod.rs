use std::sync::Arc;

use anyhow::Result;
use ratatui::{
    Terminal,
    crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    layout::Rect,
    prelude::Position,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use tokio::sync::{Mutex, mpsc};

use crate::agent::Agent;
use crate::config::Config;
use crate::hooks::HookRegistry;
use crate::hooks::audit::AuditHook;
use crate::pause::{self, AgentResume, PauseKind};
use crate::provider::StreamEvent;

pub mod markdown;
pub mod state;
pub mod theme;
pub mod views;
pub mod widgets;

use state::{AppState, ChatMessage, ChatRole};
use theme::{Palette, body};
use views::{View, render_view};

/// What session, if any, the TUI should load on startup.
#[derive(Debug, Clone)]
pub enum ResumeTarget {
    /// Start fresh — new session, empty history.
    None,
    /// Resume the most recently active session.
    Last,
    /// Resume a specific session by ID.
    Specific(String),
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
];

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
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

    // ── Long-lived agent: one per TUI session, shared across prompts so
    // hook handlers' state (audit log, rate limits, etc.) survives turns.
    let agent = Arc::new(Mutex::new(
        Agent::with_hooks_and_pause(config, hook_reg, Some(pause_tx)).await?,
    ));

    // ── Apply resume target if any. Errors here are non-fatal — we report
    // and start fresh.
    let resume_note = {
        let mut a = agent.lock().await;
        match resume {
            ResumeTarget::None => None,
            ResumeTarget::Last => match a.continue_last_session() {
                Ok(()) => Some(format!(
                    "Resumed last session ({})",
                    short_id(a.session_id())
                )),
                Err(e) => Some(format!("Could not resume last session: {e}")),
            },
            ResumeTarget::Specific(id) => match a.resume_session(&id) {
                Ok(()) => Some(format!("Resumed session {}", short_id(&id))),
                Err(e) => Some(format!("Could not resume session: {e}")),
            },
        }
    };

    {
        let a = agent.lock().await;
        a.start_session().await;
    }

    let mut app = AppState::new(
        config.provider.model.clone(),
        config.provider.max_context as u32,
    );
    app.audit_log = Some(audit_buf);
    // YYC-66: clone the agent's diff sink so the TUI can render real edits.
    // YYC-67: pull catalog pricing for the cost estimate.
    {
        let a = agent.lock().await;
        app.diff_sink = Some(a.diff_sink().clone());
        app.pricing = a.pricing().cloned();
    }
    refresh_sessions(&agent, &mut app).await;

    if let Some(note) = resume_note {
        app.messages.push(ChatMessage {
            role: ChatRole::System,
            content: note,
            reasoning: String::new(),
                            segments: Vec::new(),
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
                        });
                    }
                    Message::Tool { .. } => {} // skip — audit log shows tool activity
                }
            }
        }
    }

    let mut exit = false;
    let mut pending_quit = false;

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
                draw_palette(f, area, pal, app.slash_menu_selection);
            }
        })?;

        tokio::select! {
            pause = pause_rx.recv() => {
                if let Some(p) = pause {
                    // YYC-59: pause now carries inline pill options, so the
                    // bracket-list hint is redundant when options is present.
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
                    };
                    app.messages.push(ChatMessage {
                        role: ChatRole::System,
                        content: format!("⏸  Agent paused — {summary}"),
                        reasoning: String::new(),
                            segments: Vec::new(),
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
                                            };
                                            let _ = p.reply.send(r);
                                            app.messages.push(ChatMessage {
                                                role: ChatRole::System,
                                                content: format!("▶  Resumed — {label}"),
                                                reasoning: String::new(),
                            segments: Vec::new(),
                                            });
                                            app.note_resume(label);
                                        }
                                        continue;
                                    }
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
                                        KeyCode::Char('r') => {
                                            app.show_reasoning = !app.show_reasoning;
                                            continue;
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
                                                });
                                                pending_quit = true;
                                            } else {
                                                pending_quit = true;
                                                app.messages.push(ChatMessage {
                                                    role: ChatRole::System,
                                                    content: "Press Ctrl+C again to quit, or any key to cancel.".into(),
                                                    reasoning: String::new(),
                            segments: Vec::new(),
                                                });
                                            }
                                            continue;
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
                                                        continue;
                                                    }
                                                    "reasoning" => {
                                                        app.show_reasoning = !app.show_reasoning;
                                                        app.messages.push(ChatMessage {
                                                            role: ChatRole::System,
                                                            content: format!("Reasoning trace: {}", if app.show_reasoning { "on" } else { "off" }),
                                                            reasoning: String::new(),
                            segments: Vec::new(),
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
                                                    s if s.starts_with("view ") => {
                                                        if let Ok(n) = s[5..].trim().parse::<u8>() {
                                                            if let Some(v) = View::from_index(n) {
                                                                app.view = v;
                                                            }
                                                        }
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
                                                        });
                                                        continue;
                                                    }
                                                    _ => {
                                                        app.messages.push(ChatMessage {
                                                            role: ChatRole::System,
                                                            content: format!("Unknown command: {msg}. Try /help"),
                                                            reasoning: String::new(),
                            segments: Vec::new(),
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
                    Some(StreamEvent::Text(chunk)) => {
                        if let Some(last) = app.messages.last_mut() {
                            if matches!(last.role, ChatRole::Agent) {
                                // Append to both the segment timeline (the
                                // YYC-71 ordered renderer) and the legacy
                                // `content` field (kept so other code that
                                // peeks at .content keeps working).
                                last.append_text(&chunk);
                                last.content.push_str(&chunk);
                            }
                        }
                    }
                    Some(StreamEvent::Reasoning(chunk)) => {
                        // Per-token reasoning trace from thinking-mode models.
                        // Push to the segment timeline so it interleaves with
                        // tool calls in render order; also append to the
                        // legacy `reasoning` field so latest_reasoning() etc.
                        // continue to work.
                        if let Some(last) = app.messages.last_mut() {
                            if matches!(last.role, ChatRole::Agent) {
                                last.append_reasoning(&chunk);
                                last.reasoning.push_str(&chunk);
                            }
                        }
                        app.note_reasoning();
                    }
                    Some(StreamEvent::Done(resp)) => {
                        app.thinking = false;
                        if let Some(usage) = resp.usage {
                            // YYC-60: track lifetime totals for cost (YYC-67)
                            // and the latest prompt size for the in-status
                            // capacity bar.
                            app.prompt_tokens_total = app
                                .prompt_tokens_total
                                .saturating_add(usage.prompt_tokens as u32);
                            app.completion_tokens_total = app
                                .completion_tokens_total
                                .saturating_add(usage.completion_tokens as u32);
                            app.prompt_tokens_last = usage.prompt_tokens as u32;
                        }
                        app.note_done();
                        refresh_sessions(&agent, &mut app).await;
                        // YYC-61: drain one queued prompt per turn end. Subsequent
                        // queued prompts ride the next Done event in the same way.
                        if let Some(next) = app.queue.pop_front() {
                            submit_prompt(&mut app, &agent, &stream_tx, next);
                        }
                    }
                    Some(StreamEvent::Error(e)) => {
                        if let Some(last) = app.messages.last_mut() {
                            if last.content.is_empty() {
                                last.content = format!("⚠ Error: {e}");
                            }
                        }
                        app.thinking = false;
                        // YYC-67: record provider-level error for telemetry.
                        app.provider_errors_total = app.provider_errors_total.saturating_add(1);
                        app.note_error(&e);
                    }
                    Some(StreamEvent::ToolCallStart { name, .. }) => {
                        // Push a tool-call segment into the timeline. The
                        // renderer interleaves it between Reasoning/Text
                        // segments in arrival order (YYC-71). The legacy
                        // `content` string is left alone — its embedded
                        // `_[🔧 …]_` markers are gone since the renderer now
                        // gets tool calls from `segments`.
                        if let Some(last) = app.messages.last_mut() {
                            if matches!(last.role, ChatRole::Agent) {
                                last.push_tool_start(name.clone());
                            }
                        }
                        app.note_tool_start(&name);
                    }
                    Some(StreamEvent::ToolCallEnd { name, ok, .. }) => {
                        if let Some(last) = app.messages.last_mut() {
                            if matches!(last.role, ChatRole::Agent) {
                                last.finish_tool(&name, ok);
                            }
                        }
                        // YYC-67: tool call telemetry.
                        app.tool_calls_total = app.tool_calls_total.saturating_add(1);
                        if !ok {
                            app.tool_errors_total = app.tool_errors_total.saturating_add(1);
                        }
                        app.note_tool_end(&name, ok);
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

fn draw_palette(
    f: &mut ratatui::Frame,
    area: Rect,
    cmds: &[&SlashCommand],
    selected: usize,
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
        Paragraph::new(header_text).style(
            Style::default()
                .fg(Palette::PAPER)
                .bg(Palette::INK)
                .add_modifier(Modifier::BOLD),
        ),
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
            Style::default().fg(Palette::MUTED),
        )));
    } else {
        let active = selected.min(cmds.len().saturating_sub(1));
        for (i, cmd) in cmds.iter().enumerate() {
            let is_active = i == active;
            // YYC-70: highlight the active row with inverted accent.
            let (prefix, name_style, desc_style) = if is_active {
                (
                    "▸ ",
                    Style::default()
                        .fg(Palette::PAPER)
                        .bg(Palette::RED)
                        .add_modifier(Modifier::BOLD),
                    Style::default().fg(Palette::PAPER).bg(Palette::RED),
                )
            } else {
                (
                    "  ",
                    Style::default()
                        .fg(Palette::RED)
                        .add_modifier(Modifier::BOLD),
                    Style::default().fg(Palette::INK),
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
