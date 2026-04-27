use ratatui::{
    Frame as TuiFrame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};

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

/// Reasoning trace block — italicized, hatched bg.
///
/// The hidden-trace placeholder text uses the active theme's `muted`
/// role. The visible reasoning rows sit on the structural FAINT
/// backdrop (a Bauhaus tint that's always lighter than PAPER) — the
/// inset-card affordance is design-locked, not themed.
pub fn reasoning_lines(text: &str, hidden: bool, theme: &Theme) -> Vec<Line<'static>> {
    if hidden {
        return vec![Line::from(Span::styled(
            "░░░ reasoning trace hidden · Ctrl-R to show ░░░",
            theme.muted.add_modifier(Modifier::DIM),
        ))];
    }
    let mut lines = vec![Line::from(Span::styled(
        " ▒ THINKING",
        theme.muted.add_modifier(Modifier::BOLD),
    ))];
    for raw in text.lines() {
        lines.push(Line::from(Span::styled(
            format!(" ▒ {}", raw),
            theme.muted.add_modifier(Modifier::ITALIC),
        )));
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

/// Bottom prompt row with mode pill, red caret, the input text + cursor,
/// dashed key-hint line, and right-aligned model + token gauge.
///
/// Returns the (x, y) where the OS cursor should be placed.
///
/// YYC-52: divider, input fill, hint text, and capacity-ok status fg
/// follow the active theme. The mode pill (inverse PAPER-on-INK) and
/// the caret (RED) stay structural — they're chrome, not chat content.
/// Capacity-warning fg colors (yellow/red) stay on the design's warn
/// palette so the urgency cue is identical across themes.
pub fn prompt_row(
    f: &mut TuiFrame,
    area: Rect,
    mode: &str,
    input: &str,
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
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

    let div = "─".repeat(area.width as usize);
    f.render_widget(Paragraph::new(div).style(theme.border), layout[0]);

    let mode_pill = Span::styled(
        format!("[{}]", mode),
        Style::default()
            .fg(theme.body_fg)
            .add_modifier(Modifier::BOLD),
    );
    let caret = Span::styled(
        " ❯ ",
        Style::default()
            .fg(Palette::RED)
            .add_modifier(Modifier::BOLD),
    );
    let input_span = Span::styled(input.to_string(), body_style);
    let cursor_block = Span::styled(
        if thinking { "▒" } else { "█" },
        body_style.add_modifier(if thinking {
            Modifier::SLOW_BLINK
        } else {
            Modifier::empty()
        }),
    );
    let line = Line::from(vec![mode_pill, caret, input_span, cursor_block]);
    f.render_widget(Paragraph::new(line), layout[1]);

    let mode_width = mode.chars().count() as u16 + 2;
    let cursor_x = layout[1].x + mode_width + 3 + input.chars().count() as u16;
    let cursor_y = layout[1].y;

    let muted_style = theme.muted;
    let mut hint_spans: Vec<Span<'static>> = Vec::new();
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
        hint_spans.push(Span::styled(format!(" {label}"), muted_style));
    }
    let hint_text_len: u16 = hint_spans
        .iter()
        .map(|s| s.content.chars().count() as u16)
        .sum();
    let model_len = model_status.chars().count() as u16;
    if hint_text_len + model_len + 2 < area.width {
        let pad = " ".repeat((area.width - hint_text_len - model_len - 1) as usize);
        hint_spans.push(Span::raw(pad));
        let status_fg = if capacity_ratio > 0.90 {
            Palette::RED
        } else if capacity_ratio > 0.70 {
            Palette::YELLOW
        } else {
            theme.body_fg
        };
        hint_spans.push(Span::styled(
            format!(" {model_status}"),
            Style::default().fg(status_fg).add_modifier(Modifier::BOLD),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(hint_spans)), layout[2]);

    (cursor_x, cursor_y)
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
///
/// YYC-52: this card is **structural** per the YYC-74 design canvas —
/// the SLATE header bg, the FAINT body bg, the inverse name pill, and
/// the GREEN/YELLOW/RED status pills are design-locked. They do not
/// follow the active theme. Diff coloring inside the body preview also
/// stays on the structural diff palette so `+` / `-` lines remain
/// instantly recognizable across themes.
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

    // Border-only card. Foreground emphasis carries the visual hierarchy
    // so the active terminal background shows through and copy-paste
    // captures plain text.
    let border = Style::default().fg(Palette::MUTED);

    let mut out = vec![Line::from(vec![
        Span::styled("┌", border),
        Span::styled("─".repeat(inner_w), border),
        Span::styled("┐", border),
    ])];

    let mut header: Vec<Span<'static>> = Vec::new();
    header.push(Span::styled("│", border));
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
    header.push(Span::styled("│", border));
    out.push(Line::from(header));

    let body_indent = "  ";
    let body_inner = inner_w.saturating_sub(body_indent.chars().count());

    let render_body = |spans: &mut Vec<Span<'static>>, used: usize| {
        let pad = inner_w.saturating_sub(used);
        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad)));
        }
        spans.push(Span::styled("│", border));
    };

    if let Some(meta) = result_meta {
        let used = body_indent.chars().count() + meta.chars().count();
        let mut spans = vec![
            Span::styled("│", border),
            Span::raw(body_indent.to_string()),
            Span::styled(
                meta.to_string(),
                Style::default()
                    .fg(Palette::MUTED)
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
            let used = body_indent.chars().count() + body.chars().count();
            let mut spans = vec![
                Span::styled("│", border),
                Span::raw(body_indent.to_string()),
                Span::styled(body, body_style),
            ];
            render_body(&mut spans, used);
            out.push(Line::from(spans));
        }
    }

    if elided_lines > 0 {
        let footer = format!(
            "… {elided_lines} more line{} elided",
            if elided_lines == 1 { "" } else { "s" }
        );
        let used = body_indent.chars().count() + footer.chars().count();
        let mut spans = vec![
            Span::styled("│", border),
            Span::raw(body_indent.to_string()),
            Span::styled(
                footer,
                Style::default()
                    .fg(Palette::MUTED)
                    .add_modifier(Modifier::ITALIC),
            ),
        ];
        render_body(&mut spans, used);
        out.push(Line::from(spans));
    }

    out.push(Line::from(vec![
        Span::styled("└", border),
        Span::styled("─".repeat(inner_w), border),
        Span::styled("┘", border),
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
