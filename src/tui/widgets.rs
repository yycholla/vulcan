use ratatui::{
    Frame as TuiFrame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};
use tui_textarea::TextArea;

use super::theme::{Palette, Theme};

// widgets.rs hosts the structural shell of the TUI — frames, section
// headers, the prompt row, the ticker, the tool-call card. Backgrounds
// are deliberately omitted everywhere so the active terminal theme
// shows through and copy-paste captures plain text. Emphasis is carried
// by foreground color, bold, brackets, and box-drawing borders.

/// Framed window: thick border, title bar with `▓▓` mark + uppercase
/// title on left, status pill on right.
///
/// `accent` recolors the title-bar foreground; default is `theme.border`.
pub fn frame(
    f: &mut TuiFrame,
    area: Rect,
    title: &str,
    status: Option<&str>,
    accent: Option<Color>,
    theme: &Theme,
) -> Rect {
    if area.width < 4 || area.height < 3 {
        return area;
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(theme.border);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height == 0 {
        return inner;
    }
    let bar = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    let bar_fg = accent.unwrap_or(theme.body_fg);
    let bar_style = Style::default().fg(bar_fg).add_modifier(Modifier::BOLD);

    let mut spans = vec![
        Span::styled(" ▓▓ ", bar_style),
        Span::styled(title.to_uppercase(), bar_style),
    ];
    if let Some(s) = status {
        let status_text = format!(" {s} ");
        let used: u16 = (4 + title.chars().count() + status_text.chars().count()) as u16;
        if used < bar.width {
            let pad = " ".repeat((bar.width - used) as usize);
            spans.push(Span::raw(pad));
        }
        spans.push(Span::styled(status_text, bar_style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), bar);

    Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height.saturating_sub(1),
    }
}

/// Render a section header inside a pane: `▣ LABEL` plus an underline,
/// foreground emphasis only so the terminal background shows through.
pub fn section_header(f: &mut TuiFrame, area: Rect, label: &str, accent: Option<Color>) -> Rect {
    if area.height == 0 {
        return area;
    }
    let fg = accent.unwrap_or(Color::Reset);
    let style = Style::default().fg(fg).add_modifier(Modifier::BOLD);
    let bar = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    let text = format!(" ▣ {} ", label.to_uppercase());
    f.render_widget(Paragraph::new(text).style(style), bar);
    Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height - 1,
    }
}

/// Status pill rendered inline. Foreground-only — bracketed when
/// `filled` to keep emphasis without painting a background.
pub fn pill(text: &str, color: Color, filled: bool) -> Span<'static> {
    let body = if filled {
        format!("[{}]", text.to_uppercase())
    } else {
        format!(" {} ", text.to_uppercase())
    };
    Span::styled(
        body,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

/// Sparkline characters — 1 char per value.
pub fn sparkline(values: &[u16]) -> String {
    let chars: Vec<char> = "▁▂▃▄▅▆▇█".chars().collect();
    let max = (*values.iter().max().unwrap_or(&1)).max(1);
    values
        .iter()
        .map(|v| {
            let idx = ((*v as f32 / max as f32) * 7.0).round() as usize;
            chars[idx.min(7)]
        })
        .collect()
}

/// Tool call card — three lines: header (name + status), args, optional result.
/// Returns the lines so the caller composes them into a Paragraph.
pub fn tool_call_lines(
    name: &str,
    args: &str,
    status: ToolStatus,
    result: Option<&str>,
) -> Vec<Line<'static>> {
    let (color, label) = match status {
        ToolStatus::Ok => (Palette::GREEN, "✓ ok"),
        ToolStatus::Run => (Palette::YELLOW, "● running"),
        ToolStatus::Err => (Palette::RED, "✗ failed"),
    };
    let mut lines = Vec::new();
    let header = format!(" ┏ {name:<28}{label:>14} ");
    lines.push(Line::from(Span::styled(
        header,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        format!(" ┃ {}", args),
        Style::default().fg(Palette::MUTED),
    )));
    let tail = result
        .map(|r| format!(" ┗ {}", r))
        .unwrap_or_else(|| " ┗".to_string());
    lines.push(Line::from(Span::raw(tail)));
    lines
}

#[derive(Copy, Clone)]
pub enum ToolStatus {
    Ok,
    Run,
    Err,
}

/// Reasoning trace block — italicized text rows.
///
/// The hidden-trace placeholder text uses the active theme's `muted`
/// role. Visible reasoning stays copy-friendly: no repeated rails or
/// leading pipe characters in body rows.
pub fn reasoning_lines(text: &str, hidden: bool, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    if hidden {
        return vec![Line::from(Span::styled(
            "░░░ reasoning trace hidden · Ctrl-R to show ░░░",
            theme.muted.add_modifier(Modifier::DIM),
        ))];
    }
    let mut lines = vec![Line::from(Span::styled(
        "THINKING",
        theme.muted.add_modifier(Modifier::BOLD),
    ))];
    let inner_width = width.max(1) as usize;
    let body_style = theme.muted.add_modifier(Modifier::ITALIC);
    for raw in text.lines() {
        let chars: Vec<char> = raw.chars().collect();
        if chars.is_empty() {
            lines.push(Line::from(""));
            continue;
        }
        let mut idx = 0usize;
        while idx < chars.len() {
            let end = (idx + inner_width).min(chars.len());
            let chunk: String = chars[idx..end].iter().collect();
            lines.push(Line::from(Span::styled(chunk, body_style)));
            idx = end;
        }
    }
    lines
}

/// Message header: ▆ ROLE · tag
///
/// `accent` is the role color (caller resolves from `theme.user`,
/// `theme.assistant`, etc.). `theme.body_fg` is used for the role
/// label so it follows the active theme's foreground.
pub fn message_header(
    role: &str,
    accent: Color,
    tag: Option<&str>,
    theme: &Theme,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            "▆ ",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            role.to_uppercase(),
            Style::default()
                .fg(theme.body_fg)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(t) = tag {
        spans.push(Span::raw(" "));
        spans.push(pill(t, accent, false));
    }
    Line::from(spans)
}

/// Render a paragraph with a left accent bar — used for chat message bodies.
pub fn render_message_body(
    f: &mut TuiFrame,
    area: Rect,
    accent: Color,
    lines: Vec<Line<'static>>,
    theme: &Theme,
) {
    if area.width < 3 || area.height == 0 {
        return;
    }
    let body_style = Style::default().fg(theme.body_fg);
    let bar = Rect {
        x: area.x,
        y: area.y,
        width: 1,
        height: area.height,
    };
    f.render_widget(
        Paragraph::new(
            std::iter::repeat_with(|| Line::from(Span::styled("▎", Style::default().fg(accent))))
                .take(area.height as usize)
                .collect::<Vec<_>>(),
        ),
        bar,
    );
    let body_area = Rect {
        x: area.x + 2,
        y: area.y,
        width: area.width.saturating_sub(2),
        height: area.height,
    };
    f.render_widget(
        Paragraph::new(lines)
            .style(body_style)
            .wrap(Wrap { trim: false }),
        body_area,
    );
}

/// Bottom prompt row with metadata rail, red caret, input text + cursor,
/// and key-hint command line.
///
/// Returns the (x, y) where the OS cursor should be placed.
///
/// YYC-52: divider, input fill, hint text, and capacity-ok status fg
/// follow the active theme. The mode pill (inverse PAPER-on-INK) and
/// the caret (RED) stay structural — they're chrome, not chat content.
/// Capacity-warning fg colors (yellow/red) stay on the design's warn
/// palette so the urgency cue is identical across themes.
/// Compute total prompt-row height for a given input width + mode label.
/// Caller layouts use this to reserve enough vertical space for wrap.
/// Returns at least 3 (1 metadata row + 1 input row + 1 hints row).
pub fn prompt_row_height(input: &str, width: u16, _mode: &str) -> u16 {
    let editor_width = width.saturating_sub(2).max(1) as usize;
    let input_lines = input
        .split('\n')
        .map(|line| (line.chars().count() + 1).max(1).div_ceil(editor_width))
        .sum::<usize>()
        .max(1) as u16;
    1 + input_lines + 2 + 1
}

// YYC-275: Frame + Rect + mode + input + 5 cosmetic fields per draw
// call. A builder would allocate every frame; allowed here.
#[allow(clippy::too_many_arguments)]
pub fn prompt_row(
    f: &mut TuiFrame,
    area: Rect,
    mode: &str,
    textarea: &TextArea<'_>,
    hints: &[(String, String)],
    model_status: &str,
    // capacity_ratio (YYC-60): current context / max. Drives the
    // model_status fg color: ≤70% body_fg, 70-90% yellow, >90% red.
    capacity_ratio: f32,
    thinking: bool,
    theme: &Theme,
) -> (u16, u16) {
    if area.height < 2 {
        return (area.x, area.y);
    }
    let body_style = Style::default().fg(theme.body_fg);
    // YYC-104: input row height grows with wrap so long prompts stay
    // visible instead of scrolling off the right edge.
    let input_height = area.height.saturating_sub(2).max(3);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .split(area);

    render_prompt_meta(
        f,
        layout[0],
        mode,
        model_status,
        capacity_ratio,
        thinking,
        theme,
    );

    let mut editor = textarea.clone();
    editor.set_style(body_style);
    editor.set_cursor_line_style(Style::default());
    editor.set_cursor_style(
        body_style
            .fg(if thinking {
                Palette::YELLOW
            } else {
                Palette::RED
            })
            .add_modifier(Modifier::REVERSED),
    );
    editor.set_block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(theme.border)
            .title(Span::styled(
                " ❯ prompt ",
                Style::default()
                    .fg(Palette::RED)
                    .add_modifier(Modifier::BOLD),
            )),
    );
    f.render_widget(&editor, layout[1]);

    let (cursor_row, cursor_col) = textarea.cursor();
    let cursor_x = layout[1]
        .x
        .saturating_add(1)
        .saturating_add(cursor_col as u16)
        .min(layout[1].right().saturating_sub(2));
    let cursor_y = layout[1]
        .y
        .saturating_add(1)
        .saturating_add(cursor_row as u16)
        .min(layout[1].bottom().saturating_sub(2));

    render_prompt_hints(f, layout[2], hints, theme);

    (cursor_x, cursor_y)
}

fn render_prompt_meta(
    f: &mut TuiFrame,
    area: Rect,
    mode: &str,
    model_status: &str,
    capacity_ratio: f32,
    thinking: bool,
    theme: &Theme,
) {
    if area.width == 0 {
        return;
    }

    let status_fg = capacity_color(capacity_ratio, theme);
    let mode_color = if thinking {
        Palette::YELLOW
    } else {
        theme.body_fg
    };
    let mode_label = if thinking { "BUSY" } else { mode };
    let context = format!(
        "CTX {:>3}%",
        (capacity_ratio.clamp(0.0, 1.0) * 100.0).round() as u8
    );

    let left_text = format!("╭─ [{mode_label}]");
    let context_text = format!("  {context}");
    let right_text = format!("  {model_status}");
    let left_len = left_text.chars().count();
    let context_len = context_text.chars().count();
    let right_len = right_text.chars().count();
    let total_len = left_len + context_len + right_len;

    let mut spans = vec![
        Span::styled("╭─ ", theme.border),
        Span::styled(
            format!("[{mode_label}]"),
            Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
        ),
    ];

    if total_len <= area.width as usize {
        spans.push(Span::styled(context_text, Style::default().fg(status_fg)));
        let used = left_len + context_len + right_len;
        if used < area.width as usize {
            spans.push(Span::raw(" ".repeat(area.width as usize - used)));
        }
        spans.push(Span::styled(
            right_text,
            Style::default().fg(status_fg).add_modifier(Modifier::BOLD),
        ));
    } else if left_len + context_len <= area.width as usize {
        spans.push(Span::styled(context_text, Style::default().fg(status_fg)));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_prompt_hints(f: &mut TuiFrame, area: Rect, hints: &[(String, String)], theme: &Theme) {
    if area.width == 0 {
        return;
    }

    let mut hint_spans: Vec<Span<'static>> = vec![Span::styled("╰─ ", theme.border)];
    for (i, (key, label)) in hints.iter().enumerate() {
        if i > 0 {
            hint_spans.push(Span::raw("  "));
        }
        hint_spans.push(Span::styled(
            format!("[{key}]"),
            Style::default()
                .fg(theme.body_fg)
                .add_modifier(Modifier::BOLD),
        ));
        hint_spans.push(Span::styled(format!(" {label}"), theme.muted));
    }

    f.render_widget(Paragraph::new(Line::from(hint_spans)), area);
}

fn capacity_color(capacity_ratio: f32, theme: &Theme) -> Color {
    if capacity_ratio > 0.90 {
        Palette::RED
    } else if capacity_ratio > 0.70 {
        Palette::YELLOW
    } else {
        theme.body_fg
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{
        Terminal,
        backend::{Backend, TestBackend},
    };

    #[test]
    fn prompt_row_moves_mode_context_and_model_to_meta_row() {
        let backend = TestBackend::new(96, 5);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let theme = Theme::system();
        let textarea = TextArea::new(vec!["hello".to_string()]);
        let hints = vec![
            ("Enter".to_string(), "send".to_string()),
            ("Ctrl+T".to_string(), "tools".to_string()),
            ("Ctrl+K".to_string(), "sessions".to_string()),
            ("/".to_string(), "cmds".to_string()),
        ];
        let mut cursor = (0, 0);

        terminal
            .draw(|f| {
                cursor = prompt_row(
                    f,
                    f.area(),
                    "INSERT",
                    &textarea,
                    &hints,
                    "qwen · 512 / 128,000",
                    0.5,
                    false,
                    &theme,
                );
            })
            .expect("draw prompt");

        let body = terminal_buffer_text(terminal.backend());
        assert!(body.contains("╭─ [INSERT]  CTX  50%"));
        assert!(body.contains("qwen · 512 / 128,000"));
        assert!(body.contains("❯ prompt"));
        assert!(body.contains("hello"));
        assert!(body.contains("╰─ [Enter] send  [Ctrl+T] tools"));
        assert!(!body.contains("[INSERT] ❯"));
        assert_eq!(cursor, (1, 2));
    }

    #[test]
    fn prompt_row_height_wraps_without_mode_prefix() {
        let input = "x".repeat(20);

        assert_eq!(prompt_row_height(&input, 14, "INSERT"), 6);
    }

    fn terminal_buffer_text(backend: &TestBackend) -> String {
        let area = backend.size().expect("backend size");
        let buffer = backend.buffer();
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }
}

/// Bottom ticker strip — used in trading floor view. Scrolling text of
/// recent sub-agent activity. Foreground emphasis only.
pub fn ticker(f: &mut TuiFrame, area: Rect, cells: &[(String, String, Color)]) {
    if area.height == 0 {
        return;
    }
    let mut spans = vec![Span::styled(
        "[TICKER] ",
        Style::default()
            .fg(Palette::RED)
            .add_modifier(Modifier::BOLD),
    )];
    for (sub, msg, color) in cells {
        spans.push(Span::raw(" "));
        spans.push(Span::styled("█", Style::default().fg(*color)));
        spans.push(Span::styled(
            format!(" #{sub} "),
            Style::default().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(format!("{msg} ")));
        spans.push(Span::styled("│", Style::default().fg(Palette::MUTED)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Render a structured tool-call card per the design canvas (YYC-74).
/// Receipt-like, border-light, and copy-friendly: no vertical side
/// pipes on body rows, with status carried by glyph + foreground.
///
/// Layout (matches `Private/.../tools.jsx` T01–T14 title bars):
/// ```text
/// ╭─ × tool_name · params_summary               ✓ OK · 0.34s
///    N lines · 4.1 KB
///    output_preview line 1
///    output_preview line 2
/// ╰─
/// ```
///
/// YYC-52: this card is **structural** per the YYC-74 design canvas —
/// the SLATE header bg, the FAINT body bg, the inverse name pill, and
/// the GREEN/YELLOW/RED status pills are design-locked. They do not
/// follow the active theme. Diff coloring inside the body preview also
/// stays on the structural diff palette so `+` / `-` lines remain
/// instantly recognizable across themes.
// YYC-275: tool-card render takes 9 distinct cosmetic inputs every
// draw. Allowed here; revisit if `tool_card` grows further.
#[allow(clippy::too_many_arguments)]
pub fn tool_card(
    name: &str,
    status: super::state::ToolStatus,
    params_summary: Option<&str>,
    output_preview: Option<&str>,
    result_meta: Option<&str>,
    elided_lines: usize,
    elapsed_ms: Option<u64>,
    _accent: Color,
    width: u16,
) -> Vec<Line<'static>> {
    use super::state::ToolStatus;
    let (glyph, label, color) = match status {
        ToolStatus::InProgress => ("▶", "RUNNING", Palette::YELLOW),
        ToolStatus::Done(true) => ("✓", "OK", Palette::GREEN),
        ToolStatus::Done(false) => ("✗", "ERR", Palette::RED),
    };

    let inner_w = width.saturating_sub(3) as usize;

    // Left half of header: name pill + " · params"
    let name_pill_text = format!("× {name} ");
    let mut left_chars = name_pill_text.chars().count();
    let mut params_text = String::new();
    if let Some(p) = params_summary {
        params_text = format!("  {p}");
        left_chars += params_text.chars().count();
    }

    // Right half: status pill " ✓ OK 0.34s "
    let pill_body = match (status, elapsed_ms) {
        (ToolStatus::InProgress, _) => format!(" {glyph} {label}"),
        (_, Some(ms)) => format!(" {glyph} {label} {}", format_elapsed(ms)),
        (_, None) => format!(" {glyph} {label}"),
    };
    let right_chars = pill_body.chars().count();

    // Truncate params if pill + name + minimum gap don't fit.
    let min_gap = 2;
    if left_chars + right_chars + min_gap > inner_w && !params_text.is_empty() {
        let max_params =
            inner_w.saturating_sub(name_pill_text.chars().count() + right_chars + min_gap);
        if max_params >= 4 {
            let chars: Vec<char> = params_text.chars().collect();
            let kept: String = chars.iter().take(max_params - 1).collect();
            params_text = format!("{kept}…");
        } else {
            params_text.clear();
        }
        left_chars = name_pill_text.chars().count() + params_text.chars().count();
    }
    let gap = inner_w.saturating_sub(left_chars + right_chars);

    let border = Style::default().fg(Palette::MUTED);

    let mut header: Vec<Span<'static>> = Vec::new();
    header.push(Span::styled("╭─", border));
    header.push(Span::raw(" "));
    header.push(Span::styled(
        name_pill_text,
        Style::default().add_modifier(Modifier::BOLD),
    ));
    if !params_text.is_empty() {
        header.push(Span::raw(params_text));
    }
    if gap > 0 {
        header.push(Span::raw(" ".repeat(gap)));
    }
    header.push(Span::styled(
        pill_body,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
    let mut out = vec![Line::from(header)];

    let body_indent = "   ";
    let body_inner = inner_w.saturating_sub(body_indent.chars().count());

    if let Some(meta) = result_meta {
        let spans = vec![
            Span::raw(body_indent.to_string()),
            Span::styled(
                meta.to_string(),
                Style::default()
                    .fg(Palette::MUTED)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        out.push(Line::from(spans));
    }

    if let Some(preview) = output_preview {
        for raw in preview.lines() {
            let mut chars: Vec<char> = raw.chars().collect();
            if chars.len() > body_inner {
                chars.truncate(body_inner.saturating_sub(1));
                chars.push('…');
            }
            let body: String = chars.iter().collect();
            let body_style = if body.starts_with("+ ") || body.starts_with("+") && body != "+" {
                Style::default().fg(Palette::GREEN)
            } else if body.starts_with("- ") || body.starts_with("-") && body != "-" {
                Style::default().fg(Palette::RED)
            } else if body.starts_with("NEW FILE")
                || body.starts_with("MODIFIED")
                || body.starts_with("EDITED")
            {
                Style::default()
                    .fg(Palette::MUTED)
                    .add_modifier(Modifier::BOLD)
            } else if body.starts_with("… ") {
                Style::default()
                    .fg(Palette::MUTED)
                    .add_modifier(Modifier::ITALIC)
            } else {
                Style::default()
            };
            let spans = vec![
                Span::raw(body_indent.to_string()),
                Span::styled(body, body_style),
            ];
            out.push(Line::from(spans));
        }
    }

    if elided_lines > 0 {
        let footer = format!(
            "… {elided_lines} more line{} elided",
            if elided_lines == 1 { "" } else { "s" }
        );
        let spans = vec![
            Span::raw(body_indent.to_string()),
            Span::styled(
                footer,
                Style::default()
                    .fg(Palette::MUTED)
                    .add_modifier(Modifier::ITALIC),
            ),
        ];
        out.push(Line::from(spans));
    }

    out.push(Line::from(vec![
        Span::styled("╰─", border),
        Span::styled("─".repeat(inner_w.min(18)), border),
    ]));

    out
}

fn format_elapsed(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.2}s", (ms as f64) / 1000.0)
    } else {
        let secs = ms / 1000;
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

/// Fill a rect with a solid background color (paper by default).
pub fn fill(f: &mut TuiFrame, area: Rect, style: Style) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let blank: Vec<Line<'static>> = (0..area.height)
        .map(|_| Line::from(Span::styled(" ".repeat(area.width as usize), style)))
        .collect();
    f.render_widget(Paragraph::new(blank).style(style), area);
}
