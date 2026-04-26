use ratatui::{
    Frame as TuiFrame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};

use super::theme::{Palette, body, inverse};

/// Bauhaus framed window: thick double-line border, title bar with `▓▓` mark
/// + uppercase title on left, status pill on right. Matches the design's
/// `Frame` component (boxShadow handled with paper bg fill).
///
/// `accent` overrides the title-bar background; default is ink black.
pub fn frame(
    f: &mut TuiFrame,
    area: Rect,
    title: &str,
    status: Option<&str>,
    accent: Option<Color>,
) -> Rect {
    if area.width < 4 || area.height < 3 {
        return area;
    }

    // Outer block — paint paper background and a heavy border.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(Palette::INK).bg(Palette::PAPER))
        .style(body());
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Title bar = first line inside the border.
    if inner.height == 0 {
        return inner;
    }
    let bar = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    let bar_bg = accent.unwrap_or(Palette::INK);
    let bar_fg = if accent.is_some() {
        Palette::INK
    } else {
        Palette::PAPER
    };
    let bar_style = Style::default()
        .fg(bar_fg)
        .bg(bar_bg)
        .add_modifier(Modifier::BOLD);

    let mut spans = vec![
        Span::styled(" ▓▓ ", bar_style),
        Span::styled(title.to_uppercase(), bar_style),
    ];
    if let Some(s) = status {
        let status_text = format!(" {s} ");
        let used: u16 = (4 + title.chars().count() + status_text.chars().count()) as u16;
        if used < bar.width {
            let pad = " ".repeat((bar.width - used) as usize);
            spans.push(Span::styled(pad, bar_style));
        }
        spans.push(Span::styled(status_text, bar_style));
    } else {
        let used: u16 = (4 + title.chars().count()) as u16;
        if used < bar.width {
            let pad = " ".repeat((bar.width - used) as usize);
            spans.push(Span::styled(pad, bar_style));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)).style(bar_style), bar);

    // Body region = everything below the title bar.
    Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: inner.height.saturating_sub(1),
    }
}

/// Render a child "section" header inside a pane (one ink line, paper text).
pub fn section_header(f: &mut TuiFrame, area: Rect, label: &str, accent: Option<Color>) -> Rect {
    if area.height == 0 {
        return area;
    }
    let bg = accent.unwrap_or(Palette::INK);
    let fg = if accent.is_some() {
        Palette::INK
    } else {
        Palette::PAPER
    };
    let style = Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD);
    let bar = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    let text = format!(" ▣ {} ", label.to_uppercase());
    let pad_total = area.width as usize;
    let mut display = text.clone();
    if display.chars().count() < pad_total {
        display.push_str(&" ".repeat(pad_total - display.chars().count()));
    }
    f.render_widget(Paragraph::new(display).style(style), bar);
    Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height - 1,
    }
}

/// A status pill rendered inline as a Span (uppercase, padded).
pub fn pill(text: &str, color: Color, filled: bool) -> Span<'static> {
    let body = format!(" {} ", text.to_uppercase());
    let style = if filled {
        Style::default()
            .fg(Palette::PAPER)
            .bg(color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    };
    Span::styled(body, style)
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
        Style::default()
            .fg(color)
            .bg(Palette::FAINT)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        format!(" ┃ {}", args),
        Style::default().fg(Palette::MUTED).bg(Palette::PAPER),
    )));
    if let Some(r) = result {
        lines.push(Line::from(Span::styled(
            format!(" ┗ {}", r),
            Style::default().fg(Palette::INK).bg(Palette::PAPER),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            " ┗".to_string(),
            Style::default().fg(Palette::INK).bg(Palette::PAPER),
        )));
    }
    lines
}

#[derive(Copy, Clone)]
pub enum ToolStatus {
    Ok,
    Run,
    Err,
}

/// Reasoning trace block — italicized, hatched bg.
pub fn reasoning_lines(text: &str, hidden: bool) -> Vec<Line<'static>> {
    if hidden {
        return vec![Line::from(Span::styled(
            "░░░ reasoning trace hidden · Ctrl-R to show ░░░",
            Style::default()
                .fg(Palette::MUTED)
                .add_modifier(Modifier::DIM),
        ))];
    }
    let mut lines = vec![Line::from(Span::styled(
        " ▒ THINKING",
        Style::default()
            .fg(Palette::INK)
            .bg(Palette::FAINT)
            .add_modifier(Modifier::BOLD),
    ))];
    for raw in text.lines() {
        lines.push(Line::from(Span::styled(
            format!(" ▒ {}", raw),
            Style::default()
                .fg(Palette::INK)
                .bg(Palette::FAINT)
                .add_modifier(Modifier::ITALIC),
        )));
    }
    lines
}

/// Message header: ▆ ROLE · tag
pub fn message_header(role: &str, accent: Color, tag: Option<&str>) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            "▆ ",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            role.to_uppercase(),
            Style::default()
                .fg(Palette::INK)
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
pub fn render_message_body(f: &mut TuiFrame, area: Rect, accent: Color, lines: Vec<Line<'static>>) {
    if area.width < 3 || area.height == 0 {
        return;
    }
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
        )
        .style(body()),
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
            .style(body())
            .wrap(Wrap { trim: false }),
        body_area,
    );
}

/// Bottom prompt row with mode pill, red caret, the input text + cursor,
/// dashed key-hint line, and right-aligned model + token gauge.
///
/// Returns the (x, y) where the OS cursor should be placed.
pub fn prompt_row(
    f: &mut TuiFrame,
    area: Rect,
    mode: &str,
    input: &str,
    hints: &[(&str, &str)],
    model_status: &str,
    // capacity_ratio (YYC-60): current context / max. Drives the
    // model_status fg color: ≤70% ink, 70-90% yellow, >90% red.
    capacity_ratio: f32,
    thinking: bool,
) -> (u16, u16) {
    if area.height < 2 {
        return (area.x, area.y);
    }
    // Top divider
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    // Divider row
    let div = "─".repeat(area.width as usize);
    f.render_widget(
        Paragraph::new(div).style(Style::default().fg(Palette::INK).bg(Palette::PAPER)),
        layout[0],
    );

    // Mode pill + caret + input
    let mode_pill = Span::styled(
        format!(" {} ", mode),
        Style::default()
            .fg(Palette::PAPER)
            .bg(Palette::INK)
            .add_modifier(Modifier::BOLD),
    );
    let caret = Span::styled(
        " ❯ ",
        Style::default()
            .fg(Palette::RED)
            .bg(Palette::PAPER)
            .add_modifier(Modifier::BOLD),
    );
    let input_span = Span::styled(
        input.to_string(),
        Style::default().fg(Palette::INK).bg(Palette::PAPER),
    );
    let cursor_block = Span::styled(
        if thinking { "▒" } else { "█" },
        Style::default()
            .fg(Palette::INK)
            .bg(Palette::PAPER)
            .add_modifier(if thinking {
                Modifier::SLOW_BLINK
            } else {
                Modifier::empty()
            }),
    );
    let line = Line::from(vec![mode_pill, caret, input_span, cursor_block]);
    f.render_widget(Paragraph::new(line).style(body()), layout[1]);

    // Cursor position: after mode pill (mode width + 2 spaces) + caret (3) + input chars
    let mode_width = mode.chars().count() as u16 + 2;
    let cursor_x = layout[1].x + mode_width + 3 + input.chars().count() as u16;
    let cursor_y = layout[1].y;

    // Hints row
    let mut hint_spans: Vec<Span<'static>> = Vec::new();
    for (i, (key, label)) in hints.iter().enumerate() {
        if i > 0 {
            hint_spans.push(Span::styled(
                "  ",
                Style::default().fg(Palette::MUTED).bg(Palette::PAPER),
            ));
        }
        hint_spans.push(Span::styled(
            format!(" {key} "),
            Style::default()
                .fg(Palette::PAPER)
                .bg(Palette::INK)
                .add_modifier(Modifier::BOLD),
        ));
        hint_spans.push(Span::styled(
            format!(" {label}"),
            Style::default().fg(Palette::MUTED).bg(Palette::PAPER),
        ));
    }
    let hint_text_len: u16 = hint_spans
        .iter()
        .map(|s| s.content.chars().count() as u16)
        .sum();
    let model_len = model_status.chars().count() as u16;
    if hint_text_len + model_len + 2 < area.width {
        let pad = " ".repeat((area.width - hint_text_len - model_len - 1) as usize);
        hint_spans.push(Span::styled(
            pad,
            Style::default().fg(Palette::MUTED).bg(Palette::PAPER),
        ));
        // YYC-60: color the model_status span by context capacity ratio.
        let status_fg = if capacity_ratio > 0.90 {
            Palette::RED
        } else if capacity_ratio > 0.70 {
            Palette::YELLOW
        } else {
            Palette::INK
        };
        hint_spans.push(Span::styled(
            format!(" {model_status}"),
            Style::default()
                .fg(status_fg)
                .bg(Palette::PAPER)
                .add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(
        Paragraph::new(Line::from(hint_spans)).style(body()),
        layout[2],
    );

    (cursor_x, cursor_y)
}

/// Bottom ticker strip — used in trading floor view. Scrolling text of recent
/// sub-agent activity.
pub fn ticker(f: &mut TuiFrame, area: Rect, cells: &[(String, String, Color)]) {
    if area.height == 0 {
        return;
    }
    let mut spans = vec![Span::styled(
        " TICKER ",
        Style::default()
            .fg(Palette::PAPER)
            .bg(Palette::RED)
            .add_modifier(Modifier::BOLD),
    )];
    for (sub, msg, color) in cells {
        spans.push(Span::styled(
            " ".to_string(),
            Style::default().fg(Palette::PAPER).bg(Palette::INK),
        ));
        spans.push(Span::styled(
            "█".to_string(),
            Style::default().fg(*color).bg(Palette::INK),
        ));
        spans.push(Span::styled(
            format!(" #{sub} "),
            Style::default()
                .fg(Palette::PAPER)
                .bg(Palette::INK)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!("{msg} "),
            Style::default().fg(Palette::PAPER).bg(Palette::INK),
        ));
        spans.push(Span::styled(
            "│",
            Style::default().fg(Palette::MUTED).bg(Palette::INK),
        ));
    }
    // Pad to full width with ink bg
    let used: u16 = spans.iter().map(|s| s.content.chars().count() as u16).sum();
    if used < area.width {
        spans.push(Span::styled(
            " ".repeat((area.width - used) as usize),
            Style::default().bg(Palette::INK),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)).style(inverse()), area);
}

/// Render a structured tool-call card per the design canvas (YYC-74).
/// Single-line title bar (no surrounding box) — name pill on the
/// left, status pill on the right, body indented underneath with the
/// chat surface's left accent bar.
///
/// Layout (matches `Private/.../tools.jsx` T01–T14 title bars):
/// ```text
/// ▎ × tool_name · params_summary               ✓ OK · 0.34s
/// ▎   N lines · 4.1 KB
/// ▎   output_preview line 1
/// ▎   output_preview line 2
/// ```
pub fn tool_card(
    name: &str,
    status: super::state::ToolStatus,
    params_summary: Option<&str>,
    output_preview: Option<&str>,
    result_meta: Option<&str>,
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

    // Card has a 1-col border on left + right (`│`).
    let inner_w = width.saturating_sub(2) as usize;

    // Left half of header: name pill + " · params"
    let name_pill_text = format!(" × {name} ");
    let mut left_chars = name_pill_text.chars().count();
    let mut params_text = String::new();
    if let Some(p) = params_summary {
        params_text = format!("  {p}");
        left_chars += params_text.chars().count();
    }

    // Right half: status pill " ✓ OK 0.34s "
    let pill_body = match (status, elapsed_ms) {
        (ToolStatus::InProgress, _) => format!(" {glyph} {label} "),
        (_, Some(ms)) => format!(" {glyph} {label} {} ", format_elapsed(ms)),
        (_, None) => format!(" {glyph} {label} "),
    };
    let right_chars = pill_body.chars().count();

    // Truncate params if pill + name + minimum gap don't fit.
    let min_gap = 2;
    if left_chars + right_chars + min_gap > inner_w && !params_text.is_empty() {
        let max_params = inner_w
            .saturating_sub(name_pill_text.chars().count() + right_chars + min_gap);
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

    // Continuous border around the whole card. Header rows sit on
    // SLATE (including the top border row's bg); body rows sit on
    // FAINT (which is darker than the TUI's PAPER but lighter than
    // SLATE) so the title visually separates without ever breaking
    // the outer rectangle.
    let header_bg = Palette::SLATE;
    let body_bg = Palette::FAINT;
    let header_border = Style::default().fg(Palette::MUTED).bg(header_bg);
    let body_border = Style::default().fg(Palette::MUTED).bg(body_bg);

    // ── Top border: ┌──...──┐ on SLATE so it merges with the header.
    let mut out = vec![Line::from(vec![
        Span::styled("┌", header_border),
        Span::styled("─".repeat(inner_w), header_border),
        Span::styled("┐", header_border),
    ])];

    // ── Header content: │ pill params  ...  status_pill │ on SLATE.
    let mut header: Vec<Span<'static>> = Vec::new();
    header.push(Span::styled("│", header_border));
    header.push(Span::styled(
        name_pill_text,
        Style::default()
            .fg(Palette::PAPER)
            .bg(Palette::INK)
            .add_modifier(Modifier::BOLD),
    ));
    if !params_text.is_empty() {
        header.push(Span::styled(
            params_text,
            Style::default().fg(Palette::INK).bg(header_bg),
        ));
    }
    if gap > 0 {
        header.push(Span::styled(
            " ".repeat(gap),
            Style::default().bg(header_bg),
        ));
    }
    header.push(Span::styled(
        pill_body,
        Style::default()
            .fg(Palette::PAPER)
            .bg(color)
            .add_modifier(Modifier::BOLD),
    ));
    header.push(Span::styled("│", header_border));
    out.push(Line::from(header));

    // ── Body rows: │  text...padding  │ on FAINT.
    let body_indent = "  ";
    let body_inner = inner_w.saturating_sub(body_indent.chars().count());

    let render_body = |spans: &mut Vec<Span<'static>>, used: usize| {
        let pad = inner_w.saturating_sub(used);
        if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad), Style::default().bg(body_bg)));
        }
        spans.push(Span::styled("│", body_border));
    };

    if let Some(meta) = result_meta {
        let used = body_indent.chars().count() + meta.chars().count();
        let mut spans = vec![
            Span::styled("│", body_border),
            Span::styled(body_indent.to_string(), Style::default().bg(body_bg)),
            Span::styled(
                meta.to_string(),
                Style::default()
                    .fg(Palette::MUTED)
                    .bg(body_bg)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        render_body(&mut spans, used);
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
            let used = body_indent.chars().count() + body.chars().count();
            let mut spans = vec![
                Span::styled("│", body_border),
                Span::styled(body_indent.to_string(), Style::default().bg(body_bg)),
                Span::styled(body, Style::default().fg(Palette::INK).bg(body_bg)),
            ];
            render_body(&mut spans, used);
            out.push(Line::from(spans));
        }
    }

    // ── Bottom border on FAINT, closing the rectangle.
    out.push(Line::from(vec![
        Span::styled("└", body_border),
        Span::styled("─".repeat(inner_w), body_border),
        Span::styled("┘", body_border),
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
