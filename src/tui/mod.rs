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
use crate::provider::StreamEvent;

pub mod markdown;
pub mod state;
pub mod theme;
pub mod views;
pub mod widgets;

use state::{AppState, ChatMessage, ChatRole};
use theme::{Palette, body};
use views::{View, render_view};

enum KeyEv {
    Press(Event),
    Error(String),
}

#[derive(Debug, Clone)]
struct SlashCommand {
    name: &'static str,
    description: &'static str,
}

const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand { name: "exit", description: "Quit Vulcan" },
    SlashCommand { name: "quit", description: "Quit Vulcan" },
    SlashCommand { name: "help", description: "Show available commands" },
    SlashCommand { name: "clear", description: "Clear message history" },
    SlashCommand { name: "view", description: "Cycle to next view (or 1-5)" },
    SlashCommand { name: "reasoning", description: "Toggle reasoning trace" },
];

fn filter_commands(prefix: &str) -> Vec<&'static SlashCommand> {
    if prefix.is_empty() {
        return SLASH_COMMANDS.iter().collect();
    }
    let lower = prefix.to_lowercase();
    SLASH_COMMANDS.iter().filter(|c| c.name.starts_with(&lower)).collect()
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

pub async fn run_tui(config: &Config) -> Result<()> {
    let mut terminal = init_terminal()?;

    // keyboard
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<KeyEv>();
    let tx_keys = key_tx.clone();
    std::thread::spawn(move || loop {
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
    });

    // streaming
    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel::<StreamEvent>();

    // ── Hook registry: audit-log + (room for safety-gate, etc.). Built-in
    // hooks (skills) are registered by Agent::with_hooks itself.
    let mut hook_reg = HookRegistry::new();
    let (audit_hook, audit_buf) = AuditHook::new(200);
    hook_reg.register(audit_hook);

    // ── Long-lived agent: one per TUI session, shared across prompts so
    // hook handlers' state (audit log, rate limits, etc.) survives turns.
    let agent = Arc::new(Mutex::new(Agent::with_hooks(config, hook_reg)));
    {
        let a = agent.lock().await;
        a.start_session().await;
    }

    let mut app = AppState::new(
        config.provider.model.clone(),
        config.provider.max_context as u32,
    );
    app.audit_log = Some(audit_buf);

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

        terminal.draw(|f| {
            let area = f.area();
            let (main_area, palette_area) = if let Some(ref pal) = palette {
                let h = (pal.len() as u16 + 2).min(area.height / 2);
                (
                    Rect { x: area.x, y: area.y, width: area.width, height: area.height - h },
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
            if cx >= main_area.x && cx < main_area.x + main_area.width
                && cy >= main_area.y && cy < main_area.y + main_area.height
            {
                f.set_cursor_position(Position::new(cx, cy));
            }

            if let (Some(pal), Some(area)) = (palette.as_ref(), palette_area) {
                draw_palette(f, area, pal);
            }
        })?;

        tokio::select! {
            ev = key_rx.recv() => {
                match ev {
                    Some(KeyEv::Press(event)) => {
                        if let Event::Key(key) = event {
                            if key.kind == KeyEventKind::Press {
                                // ── view switching: Ctrl+1..5
                                if key.modifiers.contains(KeyModifiers::CONTROL) {
                                    match key.code {
                                        KeyCode::Char(c @ '1'..='5') => {
                                            if let Some(v) = View::from_index(c.to_digit(10).unwrap() as u8) {
                                                app.view = v;
                                            }
                                            continue;
                                        }
                                        KeyCode::Char('r') => {
                                            app.show_reasoning = !app.show_reasoning;
                                            continue;
                                        }
                                        KeyCode::Char('c') => {
                                            if pending_quit {
                                                exit = true;
                                            } else {
                                                pending_quit = true;
                                                app.messages.push(ChatMessage {
                                                    role: ChatRole::System,
                                                    content: "Press Ctrl+C again to quit, or any key to cancel.".into(),
                                                });
                                            }
                                            continue;
                                        }
                                        _ => {}
                                    }
                                }

                                match key.code {
                                    KeyCode::Enter => {
                                        if !app.input.is_empty() && !app.thinking {
                                            let msg = app.input.trim().to_string();
                                            app.input.clear();
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
                                                        app.messages.push(ChatMessage { role: ChatRole::System, content: help });
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
                                                    _ => {
                                                        app.messages.push(ChatMessage {
                                                            role: ChatRole::System,
                                                            content: format!("Unknown command: {msg}. Try /help"),
                                                        });
                                                        continue;
                                                    }
                                                }
                                            }

                                            app.messages.push(ChatMessage { role: ChatRole::User, content: msg.clone() });
                                            app.messages.push(ChatMessage { role: ChatRole::Agent, content: String::new() });
                                            app.thinking = true;
                                            app.scroll = 0;

                                            let tx = stream_tx.clone();
                                            let agent = agent.clone();
                                            tokio::spawn(async move {
                                                let mut a = agent.lock().await;
                                                let _ = a.run_prompt_stream(&msg, tx).await;
                                            });
                                        }
                                    }
                                    KeyCode::Char(c) => {
                                        pending_quit = false;
                                        app.input.push(c);
                                    }
                                    KeyCode::Backspace => {
                                        pending_quit = false;
                                        app.input.pop();
                                    }
                                    KeyCode::Tab => {
                                        pending_quit = false;
                                        if let Some(rest) = app.input.strip_prefix('/') {
                                            if let Some(c) = complete_slash(rest) {
                                                app.input = format!("/{c}");
                                            }
                                        }
                                    }
                                    KeyCode::Up => app.scroll = app.scroll.saturating_sub(1),
                                    KeyCode::Down => app.scroll = app.scroll.saturating_add(1),
                                    KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(10),
                                    KeyCode::PageDown => app.scroll = app.scroll.saturating_add(10),
                                    KeyCode::Esc => exit = true,
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
                                last.content.push_str(&chunk);
                            }
                        }
                    }
                    Some(StreamEvent::Done(resp)) => {
                        app.thinking = false;
                        if let Some(usage) = resp.usage {
                            app.token_used = app.token_used.saturating_add(usage.completion_tokens as u32);
                        }
                    }
                    Some(StreamEvent::Error(e)) => {
                        if let Some(last) = app.messages.last_mut() {
                            if last.content.is_empty() {
                                last.content = format!("⚠ Error: {e}");
                            }
                        }
                        app.thinking = false;
                    }
                    Some(StreamEvent::ToolCallStart { name, .. }) => {
                        if let Some(last) = app.messages.last_mut() {
                            if matches!(last.role, ChatRole::Agent) {
                                last.content.push_str(&format!("\n\n_[🔧 {name}…]_\n"));
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

fn draw_palette(
    f: &mut ratatui::Frame,
    area: Rect,
    cmds: &[&SlashCommand],
) {
    if area.height == 0 {
        return;
    }
    // Title bar
    let bar = Rect { x: area.x, y: area.y, width: area.width, height: 1 };
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
        for cmd in cmds {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  /{:<12}", cmd.name),
                    Style::default()
                        .fg(Palette::RED)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(cmd.description, Style::default().fg(Palette::INK)),
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
