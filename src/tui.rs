use crate::config::Config;
use crate::provider::StreamEvent;
use anyhow::Result;
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal,
};
use tokio::sync::mpsc;

/// Keyboard events from the background thread
enum KeyEvent {
    Press(Event),
    Error(String),
}

/// ── Markdown → ratatui renderer ──────────────────────────────────────

/// Theme colors for markdown rendering
struct MdTheme;

impl MdTheme {
    const HEADING: Color = Color::Cyan;
    const ITALIC: Color = Color::LightCyan;
    const CODE: Color = Color::Yellow;
    const LINK: Color = Color::Blue;
    const STRIKE: Color = Color::DarkGray;
    const QUOTE: Color = Color::Gray;
    const LIST_BULLET: Color = Color::Cyan;
    const HR: Color = Color::DarkGray;
    const CODE_BLOCK_BG: Color = Color::Rgb(30, 30, 40);
}

/// Render a markdown string into ratatui Lines with appropriate styling.
fn render_markdown(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code_block = false;
    let mut code_block_content: Vec<String> = Vec::new();

    for raw_line in text.lines() {
        if raw_line.trim_start().starts_with("```") {
            if in_code_block {
                // Close code block — flush accumulated lines
                let block_lines = render_code_block(&code_block_content);
                lines.extend(block_lines);
                code_block_content.clear();
                in_code_block = false;
            } else {
                in_code_block = true;
                code_block_content.clear();
            }
            continue;
        }

        if in_code_block {
            code_block_content.push(raw_line.to_string());
            continue;
        }

        let line = raw_line.trim_end();

        // Skip empty lines
        if line.is_empty() {
            lines.push(Line::from(""));
            continue;
        }

        // Headings: # through ######
        if let Some(level) = heading_level(line) {
            let content = line.trim_start_matches('#').trim();
            let spans = parse_inline(content);
            let mut styled = vec![Span::styled(
                match level {
                    1 => "# ",
                    2 => "## ",
                    3 => "### ",
                    4 => "#### ",
                    5 => "##### ",
                    _ => "###### ",
                },
                Style::default().fg(MdTheme::HEADING).add_modifier(Modifier::DIM),
            )];
            styled.extend(spans);
            lines.push(Line::from(styled));
            continue;
        }

        // Blockquote: >
        if let Some(content) = line.strip_prefix("> ") {
            let mut spans = vec![Span::styled("▎ ", MdTheme::QUOTE)];
            spans.extend(parse_inline(content));
            lines.push(Line::from(spans).style(Style::default().fg(MdTheme::QUOTE)));
            continue;
        }
        if line == ">" {
            lines.push(Line::from(Span::styled("▎", MdTheme::QUOTE)));
            continue;
        }

        // Unordered list: - or *
        if let Some(content) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            let mut spans = vec![Span::styled("• ", MdTheme::LIST_BULLET)];
            spans.extend(parse_inline(content));
            lines.push(Line::from(spans));
            continue;
        }

        // Ordered list: 1. 2. etc.
        if let Some(rest) = strip_ordered_list_prefix(line) {
            let (num_str, content) = rest;
            let mut spans = vec![Span::styled(
                format!("{}. ", num_str),
                MdTheme::LIST_BULLET,
            )];
            spans.extend(parse_inline(content));
            lines.push(Line::from(spans));
            continue;
        }

        // Horizontal rule: --- or ***
        let trimmed = line.trim();
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            lines.push(Line::from(Span::styled(
                "─".repeat(80.min(50)),
                Style::default().fg(MdTheme::HR).add_modifier(Modifier::DIM),
            )));
            continue;
        }

        // Regular paragraph
        lines.push(Line::from(parse_inline(line)));
    }

    // If we were in a code block at EOF, flush it
    if in_code_block {
        let block_lines = render_code_block(&code_block_content);
        lines.extend(block_lines);
    }

    lines
}

/// Check heading level (1-6), return None if not a heading
fn heading_level(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    let mut count = 0;
    for ch in trimmed.chars() {
        if ch == '#' {
            count += 1;
        } else if ch == ' ' {
            break;
        } else {
            return None; // `#text` without space is not a heading
        }
    }
    if count >= 1 && count <= 6 {
        Some(count)
    } else {
        None
    }
}

/// Strip ordered list prefix like "1." or "12." and return (number_str, rest)
fn strip_ordered_list_prefix(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && i < bytes.len() && bytes[i] == b'.' {
        let num_str = &line[..i];
        let rest = line[i + 1..].trim();
        Some((num_str, rest))
    } else {
        None
    }
}

/// Render a code block — each line gets a background tint
fn render_code_block(lines: &[String]) -> Vec<Line<'static>> {
    let mut result = Vec::new();
    if lines.is_empty() {
        result.push(Line::from(Span::styled(
            " ```",
            Style::default().fg(MdTheme::CODE).add_modifier(Modifier::DIM),
        )));
        return result;
    }
    for line in lines {
        result.push(Line::from(Span::styled(
            format!(" │{}", line),
            Style::default()
                .fg(MdTheme::CODE)
                .bg(MdTheme::CODE_BLOCK_BG),
        )));
    }
    result
}

/// Parse inline markdown elements within a line of text.
/// Handles: **bold**, *italic*, `code`, [text](url), ~~strike~~
/// Returns a Vec of styled Spans.
fn parse_inline(text: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Escape: \X
        if chars[i] == '\\' && i + 1 < len {
            spans.push(Span::raw(chars[i + 1].to_string()));
            i += 2;
            continue;
        }

        // Inline code: `code`
        if chars[i] == '`' {
            let start = i + 1;
            if let Some(end) = chars[start..].iter().position(|&c| c == '`') {
                let code: String = chars[start..start + end].iter().collect();
                spans.push(Span::styled(
                    code,
                    Style::default().fg(MdTheme::CODE).bg(Color::Rgb(25, 25, 35)),
                ));
                i = start + end + 1;
                continue;
            }
        }

        // Bold: **text**
        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            let start = i + 2;
            if let Some(end) = chars[start..].windows(2).position(|w| w == ['*', '*']) {
                let inner: String = chars[start..start + end].iter().collect();
                // Recursively parse inner for italic
                let inner_spans = parse_inline(&inner);
                let styled: Vec<Span> = inner_spans
                    .into_iter()
                    .map(|s| {
                        let mut style = s.style.clone();
                        style = style.add_modifier(Modifier::BOLD);
                        Span::styled(s.content.clone(), style)
                    })
                    .collect();
                spans.extend(styled);
                i = start + end + 2;
                continue;
            }
        }

        // Italic: *text* (only if not followed by another *)
        if chars[i] == '*' && (i + 1 >= len || chars[i + 1] != '*') {
            let start = i + 1;
            if let Some(end) = chars[start..].iter().position(|&c| c == '*') {
                let inner: String = chars[start..start + end].iter().collect();
                spans.push(Span::styled(
                    inner,
                    Style::default().fg(MdTheme::ITALIC).add_modifier(Modifier::ITALIC),
                ));
                i = start + end + 1;
                continue;
            }
        }

        // Strikethrough: ~~text~~
        if i + 1 < len && chars[i] == '~' && chars[i + 1] == '~' {
            let start = i + 2;
            if let Some(end) = chars[start..].windows(2).position(|w| w == ['~', '~']) {
                let inner: String = chars[start..start + end].iter().collect();
                spans.push(Span::styled(
                    inner,
                    Style::default()
                        .fg(MdTheme::STRIKE)
                        .add_modifier(Modifier::CROSSED_OUT),
                ));
                i = start + end + 2;
                continue;
            }
        }

        // Link: [text](url)
        if chars[i] == '[' {
            let start = i + 1;
            if let Some(close_bracket) = chars[start..].iter().position(|&c| c == ']') {
                let text: String = chars[start..start + close_bracket].iter().collect();
                let after_close = start + close_bracket + 1;
                if after_close < len && chars[after_close] == '(' {
                    let url_start = after_close + 1;
                    if let Some(close_paren) = chars[url_start..].iter().position(|&c| c == ')') {
                        let _url: String = chars[url_start..url_start + close_paren].iter().collect();
                        // Render link text in blue with underline
                        let inner_spans = parse_inline(&text);
                        let styled: Vec<Span> = inner_spans
                            .into_iter()
                            .map(|s| {
                                Span::styled(
                                    s.content.clone(),
                                    Style::default()
                                        .fg(MdTheme::LINK)
                                        .add_modifier(Modifier::UNDERLINED),
                                )
                            })
                            .collect();
                        spans.extend(styled);
                        i = url_start + close_paren + 1;
                        continue;
                    }
                }
            }
        }

        // Regular character
        spans.push(Span::raw(chars[i].to_string()));
        i += 1;
    }

    spans
}

/// ── TUI main loop ────────────────────────────────────────────────────

/// Run the interactive TUI mode with streaming response display
pub async fn run_tui(config: &Config) -> Result<()> {
    let mut terminal = init_terminal()?;

    // Channel 1: keyboard events from background thread
    let (key_tx, mut key_rx) = mpsc::unbounded_channel::<KeyEvent>();

    // Spawn a background thread to read keyboard events (crossterm is blocking)
    let tx_keys = key_tx.clone();
    std::thread::spawn(move || loop {
        match event::read() {
            Ok(ev) => {
                if tx_keys.send(KeyEvent::Press(ev)).is_err() {
                    break;
                }
            }
            Err(e) => {
                let _ = tx_keys.send(KeyEvent::Error(e.to_string()));
                break;
            }
        }
    });

    // Channel 2: streaming tokens from the agent
    let (stream_tx, mut stream_rx) = mpsc::unbounded_channel::<StreamEvent>();

    let mut input = String::new();
    let mut messages: Vec<(String, String)> = Vec::new();
    let mut scroll: usize = 0;
    let mut exit = false;
    let mut thinking = false; // true while agent is generating

    while !exit {
        terminal.draw(|f| {
            draw_ui(f, &messages, &input, scroll);
        })?;

        tokio::select! {
            // Keyboard events
            ev = key_rx.recv() => {
                match ev {
                    Some(KeyEvent::Press(event)) => {
                        if let Event::Key(key) = event {
                            if key.kind == KeyEventKind::Press {
                                match key.code {
                                    KeyCode::Enter => {
                                        if !input.is_empty() && !thinking {
                                            let msg = input.trim().to_string();
                                            input.clear();
                                            if msg == "/exit" || msg == "/quit" {
                                                exit = true;
                                                continue;
                                            }
                                            messages.push(("You".into(), msg.clone()));
                                            messages.push(("Ferris".into(), String::new()));
                                            thinking = true;
                                            scroll = 0;

                                            // Spawn agent with streaming
                                            let tx = stream_tx.clone();
                                            let config = config.clone();
                                            tokio::spawn(async move {
                                                let mut agent = crate::agent::Agent::new(&config);
                                                let _ = agent.run_prompt_stream(&msg, tx).await;
                                            });
                                        }
                                    }
                                    KeyCode::Char(c) => input.push(c),
                                    KeyCode::Backspace => { input.pop(); }
                                    KeyCode::Up => scroll = scroll.saturating_sub(1),
                                    KeyCode::Down => {
                                        scroll = (scroll + 1).min(messages.len().saturating_sub(1));
                                    }
                                    KeyCode::Esc => exit = true,
                                    _ => {}
                                }
                            }
                        }
                    }
                    Some(KeyEvent::Error(e)) => {
                        tracing::error!("Terminal input error: {e}");
                        exit = true;
                    }
                    None => exit = true,
                }
            }
            // Stream events from the agent
            ev = stream_rx.recv() => {
                match ev {
                    Some(StreamEvent::Text(chunk)) => {
                        // Append to the last "Ferris" message
                        if let Some(last) = messages.last_mut() {
                            if last.0 == "Ferris" {
                                last.1.push_str(&chunk);
                            }
                        }
                    }
                    Some(StreamEvent::Done(_)) => {
                        thinking = false;
                    }
                    Some(StreamEvent::Error(e)) => {
                        if let Some(last) = messages.last_mut() {
                            if last.1.is_empty() {
                                last.1 = format!("⚠ Error: {e}");
                            }
                        }
                        thinking = false;
                    }
                    Some(StreamEvent::ToolCallStart { name, .. }) => {
                        if let Some(last) = messages.last_mut() {
                            if last.0 == "Ferris" {
                                last.1.push_str(&format!("\n\n[🔧 Using tool: {name}...]"));
                            }
                        }
                    }
                    None => {
                        thinking = false;
                    }
                }
            }
        }
    }

    restore_terminal()?;
    Ok(())
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

fn draw_ui(
    f: &mut ratatui::Frame,
    messages: &[(String, String)],
    input: &str,
    scroll: usize,
) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(area);

    // ── Messages panel ────────────────────────────────────────────
    let msg_block = Block::default()
        .title(" Ferris ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = msg_block.inner(chunks[0]);
    f.render_widget(msg_block, chunks[0]);

    let mut lines: Vec<Line> = Vec::new();

    for (role, content) in messages.iter().rev().skip(scroll).rev() {
        match role.as_str() {
            "You" => {
                // User messages stay plain
                let style = Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD);
                let role_span = Span::styled("You: ", style);
                lines.push(Line::from(vec![
                    role_span,
                    Span::styled(content.clone(), Style::default().fg(Color::White)),
                ]));
                lines.push(Line::from(""));
            }
            _ if content.is_empty() && role == "Ferris" => {
                // Empty → "Thinking..." with blink
                lines.push(Line::from(vec![
                    Span::styled(
                        "Ferris: ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "Thinking...",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::SLOW_BLINK),
                    ),
                ]));
                lines.push(Line::from(""));
            }
            _ => {
                // Ferris message — render with markdown
                let role_span = Span::styled(
                    "Ferris: ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                );
                let mut rendered = render_markdown(content);
                if let Some(first) = rendered.first_mut() {
                    // Merge role span into first line
                    let mut new_spans = vec![role_span];
                    new_spans.extend(first.spans.drain(..));
                    first.spans = new_spans;
                } else {
                    rendered.push(Line::from(vec![role_span]));
                }
                lines.extend(rendered);
                lines.push(Line::from(""));
            }
        }
    }

    // Welcome message when empty
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "Welcome to Ferris! Type a message to start.\n\n/exit or /quit to quit.",
            Style::default().fg(Color::DarkGray),
        )));
    }

    // Render the message area
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .block(Block::default().borders(Borders::NONE))
            .wrap(Wrap { trim: false }),
        inner,
    );

    // ── Input bar ─────────────────────────────────────────────────
    let input_block = Block::default()
        .title(" Input ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    f.render_widget(
        Paragraph::new(input.to_string())
            .block(input_block)
            .style(Style::default().fg(Color::White)),
        chunks[1],
    );

    f.set_cursor_position(ratatui::prelude::Position::new(
        chunks[1].x + 1 + input.len() as u16,
        chunks[1].y + 1,
    ));
}
