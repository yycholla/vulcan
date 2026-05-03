mod prompt;
mod provider_picker;
mod ticker;

pub use prompt::{PromptRowWidget, prompt_row_height};
pub use provider_picker::ProviderPickerWidget;
pub use ticker::TickerWidget;

use ratatui::{
    Frame as TuiFrame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};

use super::theme::{Palette, Theme};

// The widgets module hosts reusable Ratatui elements. Larger elements live
// in sibling files; this mod keeps small shared chrome helpers. Backgrounds
// are deliberately omitted where possible so the active terminal theme shows
// through and copy-paste captures plain text.

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
