use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Gauge, Paragraph, Widget},
};
use throbber_widgets_tui::{BRAILLE_SIX, Throbber, ThrobberState, WhichUse};
use tui_textarea::TextArea;

use crate::tui::{
    effects::TuiEffects,
    theme::{Palette, Theme},
};

/// Compute total prompt-row height for a given input width + mode label.
/// Caller layouts use this to reserve enough vertical space for wrap.
/// Returns at least 3 rows: top border + editor body + bottom border.
pub fn prompt_row_height(input: &str, width: u16, _mode: &str) -> u16 {
    let editor_width = width.saturating_sub(2).max(1) as usize;
    let input_lines = input
        .split('\n')
        .map(|line| (line.chars().count() + 1).max(1).div_ceil(editor_width))
        .sum::<usize>()
        .max(1) as u16;
    input_lines + 2
}

pub struct PromptRowWidget<'a> {
    pub mode: &'a str,
    pub textarea: &'a TextArea<'a>,
    pub hints: &'a [(String, String)],
    pub model_status: &'a str,
    pub capacity_ratio: f32,
    pub thinking: bool,
    pub activity_active: bool,
    pub activity_throbber: Option<&'a ThrobberState>,
    pub effects: Option<&'a TuiEffects>,
    pub theme: &'a Theme,
}

impl PromptRowWidget<'_> {
    pub fn cursor(&self, area: Rect) -> (u16, u16) {
        if area.height < 2 {
            return (area.x, area.y);
        }
        let editor_area = prompt_body_area(area);
        let (cursor_row, cursor_col) = self.textarea.cursor();
        let cursor_x = editor_area
            .x
            .saturating_add(cursor_col as u16)
            .min(editor_area.right().saturating_sub(1));
        let cursor_y = editor_area
            .y
            .saturating_add(cursor_row as u16)
            .min(editor_area.bottom().saturating_sub(1));
        (cursor_x, cursor_y)
    }
}

impl Widget for PromptRowWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height < 2 {
            return;
        }
        let status_parts = prompt_status_parts(self.model_status);
        let body_style = Style::default().fg(self.theme.body_fg);
        render_prompt_border(
            buf,
            area,
            self.mode,
            self.hints,
            self.capacity_ratio,
            status_parts.token_usage.as_ref(),
            self.thinking,
            self.activity_active,
            self.activity_throbber,
            self.effects.map(TuiEffects::prompt_border_phase),
            self.theme,
        );

        let mut editor = self.textarea.clone();
        editor.set_style(body_style);
        editor.set_cursor_line_style(Style::default());
        editor.set_cursor_style(
            body_style
                .fg(if self.thinking {
                    Palette::YELLOW
                } else {
                    Palette::RED
                })
                .add_modifier(Modifier::REVERSED),
        );
        (&editor).render(prompt_body_area(area), buf);

        render_model_status(
            buf,
            prompt_body_area(area),
            self.textarea,
            &status_parts.model_tag,
            self.thinking,
            self.theme,
        );
    }
}

fn prompt_body_area(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

fn render_prompt_border(
    buf: &mut Buffer,
    area: Rect,
    mode: &str,
    hints: &[(String, String)],
    capacity_ratio: f32,
    token_usage: Option<&TokenUsageLabel>,
    thinking: bool,
    activity_active: bool,
    activity_throbber: Option<&ThrobberState>,
    border_phase: Option<u16>,
    theme: &Theme,
) {
    if area.width < 2 || area.height < 2 {
        return;
    }

    let status_fg = capacity_color(capacity_ratio, theme);
    let mode_color = if thinking {
        Palette::YELLOW
    } else {
        theme.body_fg
    };
    let mode_label = if thinking { "BUSY" } else { mode };
    let border_style = if thinking {
        Style::default()
            .fg(Palette::YELLOW)
            .add_modifier(Modifier::BOLD)
    } else {
        theme.border
    };

    for x in area.x..area.right() {
        buf[(x, area.y)].set_symbol("─").set_style(border_style);
        buf[(x, area.bottom() - 1)]
            .set_symbol("─")
            .set_style(border_style);
    }
    for y in area.y + 1..area.bottom() - 1 {
        buf[(area.x, y)].set_symbol("│").set_style(border_style);
        buf[(area.right() - 1, y)]
            .set_symbol("│")
            .set_style(border_style);
    }
    buf[(area.x, area.y)]
        .set_symbol("┌")
        .set_style(border_style);
    buf[(area.right() - 1, area.y)]
        .set_symbol("┐")
        .set_style(border_style);
    buf[(area.x, area.bottom() - 1)]
        .set_symbol("└")
        .set_style(border_style);
    buf[(area.right() - 1, area.bottom() - 1)]
        .set_symbol("┘")
        .set_style(border_style);

    if thinking && activity_active {
        render_busy_border_sweep(buf, area, border_phase.unwrap_or_default());
    }

    let mode_style = Style::default().fg(mode_color).add_modifier(Modifier::BOLD);
    let mut label_spans = vec![Span::styled(" ❯ ", Style::default().fg(Palette::RED))];
    if thinking
        && activity_active
        && let Some(throbber_state) = activity_throbber
    {
        label_spans.push(Span::styled("[", mode_style));
        label_spans.push(
            Throbber::default()
                .throbber_set(BRAILLE_SIX)
                .use_type(WhichUse::Spin)
                .style(mode_style)
                .throbber_style(mode_style)
                .to_symbol_span(throbber_state),
        );
        label_spans.push(Span::styled(format!("{mode_label}] "), mode_style));
    } else {
        label_spans.push(Span::styled(format!("[{mode_label}] "), mode_style));
    }
    let label = Line::from(label_spans);
    Paragraph::new(label).render(
        Rect {
            x: area.x.saturating_add(1),
            y: area.y,
            width: area.width.saturating_sub(2),
            height: 1,
        },
        buf,
    );

    render_capacity_gauge(buf, area, capacity_ratio, token_usage, status_fg, theme);
    render_bottom_hints(buf, area, hints, theme);
}

fn render_busy_border_sweep(buf: &mut Buffer, area: Rect, phase: u16) {
    let perimeter = border_perimeter_len(area);
    if perimeter == 0 {
        return;
    }

    let sweep_len = (perimeter / 8).clamp(4, 12);
    let head = (phase as usize * 2) % perimeter;
    for offset in 0..sweep_len {
        let index = (head + perimeter - offset - 1) % perimeter;
        let style = match offset {
            0 => Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            1..=3 => Style::default()
                .fg(Palette::YELLOW)
                .add_modifier(Modifier::BOLD),
            _ => Style::default().fg(Palette::MUTED),
        };
        let Some((x, y)) = border_position(area, index) else {
            continue;
        };
        buf[(x, y)].set_style(style);
    }
}

fn border_perimeter_len(area: Rect) -> usize {
    if area.width < 2 || area.height < 2 {
        return 0;
    }
    (area.width as usize * 2) + (area.height.saturating_sub(2) as usize * 2)
}

fn border_position(area: Rect, index: usize) -> Option<(u16, u16)> {
    let top = area.width as usize;
    let right = area.height.saturating_sub(2) as usize;
    let bottom = area.width as usize;
    let left = right;

    if index < top {
        return Some((area.x + index as u16, area.y));
    }
    let index = index - top;
    if index < right {
        return Some((area.right() - 1, area.y + 1 + index as u16));
    }
    let index = index - right;
    if index < bottom {
        return Some((area.right() - 1 - index as u16, area.bottom() - 1));
    }
    let index = index - bottom;
    if index < left {
        return Some((area.x, area.bottom() - 2 - index as u16));
    }
    None
}

fn render_capacity_gauge(
    buf: &mut Buffer,
    area: Rect,
    capacity_ratio: f32,
    token_usage: Option<&TokenUsageLabel>,
    status_fg: Color,
    theme: &Theme,
) {
    if area.width < 54 {
        return;
    }

    let ratio = capacity_ratio.clamp(0.0, 1.0);
    let pct = (ratio * 100.0).round() as u8;
    let used_label = token_usage.map(|usage| usage.used.as_str());
    let max_label = token_usage.map(|usage| usage.max.as_str());
    let used_width = used_label.map(str_width).unwrap_or_default();
    let max_width = max_label.map(str_width).unwrap_or_default();
    let gauge_width = area.width.clamp(16, 30) / 2 + 10;
    let group_width = used_width + usize::from(gauge_width) + max_width + 4;
    let group_x = area
        .right()
        .saturating_sub(1)
        .saturating_sub(group_width as u16);
    if group_x <= area.x + 14 {
        return;
    }
    let mut cursor_x = group_x;
    if let Some(label) = used_label {
        render_inline_text(buf, cursor_x, area.y, label, Style::default().fg(status_fg));
        cursor_x = cursor_x.saturating_add(used_width as u16 + 1);
    }
    let gauge_x = cursor_x;
    buf[(gauge_x - 1, area.y)]
        .set_symbol("<")
        .set_style(theme.border);
    buf[(gauge_x + gauge_width, area.y)]
        .set_symbol(">")
        .set_style(theme.border);
    for x in gauge_x..gauge_x + gauge_width {
        buf[(x, area.y)]
            .set_symbol(" ")
            .set_style(Style::default().fg(theme.body_fg).bg(Color::DarkGray));
    }
    Gauge::default()
        .ratio(ratio as f64)
        .label("")
        .style(Style::default().fg(theme.body_fg).bg(Color::DarkGray))
        .gauge_style(
            Style::default()
                .fg(status_fg)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .use_unicode(false)
        .render(
            Rect {
                x: gauge_x,
                y: area.y,
                width: gauge_width,
                height: 1,
            },
            buf,
        );
    render_gauge_percent_label(buf, gauge_x, area.y, gauge_width, ratio, pct, status_fg);
    if let Some(label) = max_label {
        let max_x = gauge_x.saturating_add(gauge_width).saturating_add(2);
        render_inline_text(buf, max_x, area.y, label, Style::default().fg(status_fg));
    }
}

fn render_gauge_percent_label(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    ratio: f32,
    pct: u8,
    fill_color: Color,
) {
    let label = format!("{pct:>3}%");
    let label_width = str_width(&label) as u16;
    if label_width == 0 || label_width > width {
        return;
    }
    let label_x = x + (width - label_width) / 2;
    let filled_end = x + (f32::from(width) * ratio).round() as u16;
    for (offset, ch) in label.chars().enumerate() {
        let cell_x = label_x + offset as u16;
        let bg = if cell_x < filled_end {
            fill_color
        } else {
            Color::DarkGray
        };
        buf[(cell_x, y)]
            .set_symbol(&ch.to_string())
            .set_fg(Palette::INK)
            .set_bg(bg)
            .set_style(
                Style::default()
                    .fg(Palette::INK)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            );
    }
}

fn render_bottom_hints(buf: &mut Buffer, area: Rect, hints: &[(String, String)], theme: &Theme) {
    if area.width < 16 {
        return;
    }
    let y = area.bottom() - 1;
    let action = "<Enter>";
    render_text_right(buf, area, y, action, Style::default().fg(theme.body_fg));

    let hint_text = compact_hints(hints);
    if hint_text.is_empty() {
        return;
    }
    let wrapped = format!("<{hint_text}>");
    let action_len = action.chars().count() as u16;
    let hint_len = wrapped.chars().count() as u16;
    let right_limit = area.right().saturating_sub(action_len + 3);
    if hint_len + 2 >= area.width || right_limit <= area.x + hint_len + 1 {
        return;
    }
    let x = right_limit.saturating_sub(hint_len + 1);
    Paragraph::new(Line::from(vec![
        Span::styled("<", theme.border),
        Span::styled(hint_text, theme.muted),
        Span::styled(">", theme.border),
    ]))
    .render(
        Rect {
            x,
            y,
            width: hint_len,
            height: 1,
        },
        buf,
    );
}

fn render_model_status(
    buf: &mut Buffer,
    area: Rect,
    textarea: &TextArea<'_>,
    model_status: &str,
    thinking: bool,
    theme: &Theme,
) {
    if area.width < 24 || area.height == 0 || model_status.is_empty() {
        return;
    }
    let first_line_len = textarea
        .lines()
        .first()
        .map(|line| line.chars().count())
        .unwrap_or_default() as u16;
    let tag = format!(" {model_status} ");
    let tag_len = tag.chars().count() as u16;
    if first_line_len.saturating_add(tag_len).saturating_add(2) >= area.width {
        return;
    }
    let style = if thinking {
        Style::default()
            .fg(Palette::YELLOW)
            .add_modifier(Modifier::BOLD)
    } else {
        theme.muted
    };
    render_text_right(buf, area, area.y, &tag, style);
}

fn render_text_right(buf: &mut Buffer, area: Rect, y: u16, text: &str, style: Style) {
    let text_len = text.chars().count() as u16;
    if text_len >= area.width {
        return;
    }
    let x = area.right().saturating_sub(text_len + 1);
    Paragraph::new(Line::from(Span::styled(text.to_string(), style))).render(
        Rect {
            x,
            y,
            width: text_len,
            height: 1,
        },
        buf,
    );
}

fn render_inline_text(buf: &mut Buffer, x: u16, y: u16, text: &str, style: Style) {
    Paragraph::new(Line::from(Span::styled(text.to_string(), style))).render(
        Rect {
            x,
            y,
            width: str_width(text) as u16,
            height: 1,
        },
        buf,
    );
}

fn compact_hints(hints: &[(String, String)]) -> String {
    hints
        .iter()
        .filter(|(key, _)| key != "Enter" && key != "/")
        .take(2)
        .map(|(key, label)| format!("[{key}] {label}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, PartialEq, Eq)]
struct PromptStatusParts {
    model_tag: String,
    token_usage: Option<TokenUsageLabel>,
}

#[derive(Debug, PartialEq, Eq)]
struct TokenUsageLabel {
    used: String,
    max: String,
}

fn prompt_status_parts(model_status: &str) -> PromptStatusParts {
    let mut model_parts = Vec::new();
    let mut token_usage = None;

    for part in model_status.split(" · ") {
        if token_usage.is_none() {
            token_usage = token_usage_label(part);
            if token_usage.is_some() {
                continue;
            }
        }
        model_parts.push(part);
    }

    PromptStatusParts {
        model_tag: model_parts.join(" · "),
        token_usage,
    }
}

fn token_usage_label(part: &str) -> Option<TokenUsageLabel> {
    let (used, max) = part.split_once(" / ")?;
    let used = parse_token_count(used)?;
    let max = parse_token_count(max)?;
    Some(TokenUsageLabel {
        used: abbreviate_token_count(used),
        max: abbreviate_token_count(max),
    })
}

fn parse_token_count(value: &str) -> Option<u64> {
    let digits = value
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .collect::<String>();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

fn abbreviate_token_count(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}m", value as f64 / 1_000_000.0)
    } else if value >= 100_000 {
        format!("{:.0}k", value as f64 / 1_000.0)
    } else if value >= 1_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

fn str_width(value: &str) -> usize {
    value.chars().count()
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
    fn prompt_row_renders_integrated_border_status_and_hints() {
        let backend = TestBackend::new(96, 3);
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
                let widget = PromptRowWidget {
                    mode: "INSERT",
                    textarea: &textarea,
                    hints: &hints,
                    model_status: "qwen · 21,759 / 1,048,576",
                    capacity_ratio: 0.5,
                    thinking: false,
                    activity_active: false,
                    activity_throbber: None,
                    effects: None,
                    theme: &theme,
                };
                cursor = widget.cursor(f.area());
                f.render_widget(widget, f.area());
            })
            .expect("draw prompt");

        let body = terminal_buffer_text(terminal.backend());
        assert!(body.contains("┌ ❯ [INSERT]"));
        assert!(body.contains("21.8k"));
        assert!(body.contains("1.0m"));
        assert!(body.contains(" 50%"));
        assert!(!body.contains("21.8k/1.0m"));
        assert!(body.contains("qwen"));
        assert!(!body.contains("21,759 / 1,048,576"));
        assert!(body.contains("hello"));
        assert!(body.contains("<[Ctrl+T] tools [Ctrl+K] sessions>"));
        assert!(body.contains("<Enter>"));
        assert!(!body.contains("❯ prompt"));
        assert!(!body.contains("[Enter] send"));
        assert!(!body.contains("[INSERT] ❯"));
        assert_eq!(cursor, (1, 1));
    }

    #[test]
    fn prompt_row_keeps_percent_label_on_filled_gauge_background() {
        let backend = TestBackend::new(96, 3);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let theme = Theme::system();
        let textarea = TextArea::new(vec![String::new()]);
        let hints = vec![("Enter".to_string(), "send".to_string())];

        terminal
            .draw(|f| {
                f.render_widget(
                    PromptRowWidget {
                        mode: "INSERT",
                        textarea: &textarea,
                        hints: &hints,
                        model_status: "qwen · 236,000 / 262,000",
                        capacity_ratio: 0.90,
                        thinking: false,
                        activity_active: false,
                        activity_throbber: None,
                        effects: None,
                        theme: &theme,
                    },
                    f.area(),
                );
            })
            .expect("draw prompt");

        let body = terminal_buffer_text(terminal.backend());
        assert!(body.contains(" 90%"));
        assert_gauge_label_bg(terminal.backend(), "90%", Palette::YELLOW);
    }

    #[test]
    fn prompt_row_hides_model_tag_when_input_would_collide() {
        let backend = TestBackend::new(36, 3);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let theme = Theme::system();
        let textarea = TextArea::new(vec!["this prompt is already long".to_string()]);
        let hints = vec![
            ("Enter".to_string(), "send".to_string()),
            ("Ctrl+T".to_string(), "tools".to_string()),
        ];

        terminal
            .draw(|f| {
                f.render_widget(
                    PromptRowWidget {
                        mode: "INSERT",
                        textarea: &textarea,
                        hints: &hints,
                        model_status: "provider · lab/model",
                        capacity_ratio: 0.5,
                        thinking: true,
                        activity_active: false,
                        activity_throbber: None,
                        effects: None,
                        theme: &theme,
                    },
                    f.area(),
                );
            })
            .expect("draw prompt");

        let body = terminal_buffer_text(terminal.backend());
        assert!(body.contains("this prompt is already long"));
        assert!(!body.contains("provider · lab/model"));
    }

    #[test]
    fn prompt_row_animates_busy_state_with_braille_six_throbber() {
        let backend = TestBackend::new(96, 3);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let theme = Theme::system();
        let textarea = TextArea::new(vec![String::new()]);
        let hints = vec![("Enter".to_string(), "send".to_string())];
        let mut throbber_state = ThrobberState::default();
        throbber_state.calc_next();

        terminal
            .draw(|f| {
                f.render_widget(
                    PromptRowWidget {
                        mode: "INSERT",
                        textarea: &textarea,
                        hints: &hints,
                        model_status: "qwen",
                        capacity_ratio: 0.5,
                        thinking: true,
                        activity_active: true,
                        activity_throbber: Some(&throbber_state),
                        effects: None,
                        theme: &theme,
                    },
                    f.area(),
                );
            })
            .expect("draw prompt");

        let body = terminal_buffer_text(terminal.backend());
        assert!(body.contains("[⠯ BUSY]"));
        assert!(!body.contains("[BUSY]"));
    }

    #[test]
    fn prompt_row_busy_state_colors_border_and_renders_sweep() {
        let backend = TestBackend::new(64, 3);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let theme = Theme::system();
        let textarea = TextArea::new(vec![String::new()]);
        let hints = vec![("Enter".to_string(), "send".to_string())];
        let effects = TuiEffects::default();
        for _ in 0..40 {
            effects.advance_prompt_border_sweep();
        }

        terminal
            .draw(|f| {
                f.render_widget(
                    PromptRowWidget {
                        mode: "INSERT",
                        textarea: &textarea,
                        hints: &hints,
                        model_status: "qwen",
                        capacity_ratio: 0.5,
                        thinking: true,
                        activity_active: true,
                        activity_throbber: None,
                        effects: Some(&effects),
                        theme: &theme,
                    },
                    f.area(),
                );
            })
            .expect("draw prompt");

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(63, 0)].fg, Palette::YELLOW);
        assert_eq!(buffer[(49, 2)].fg, Color::White);
    }

    #[test]
    fn prompt_row_height_wraps_without_mode_prefix() {
        let input = "x".repeat(20);

        assert_eq!(prompt_row_height(&input, 14, "INSERT"), 4);
    }

    #[test]
    fn prompt_status_parts_moves_token_usage_to_capacity_label() {
        assert_eq!(
            prompt_status_parts(
                "qwen3-turboquant · deepseek/deepseek-v4-flash · 21,759 / 1,048,576"
            ),
            PromptStatusParts {
                model_tag: "qwen3-turboquant · deepseek/deepseek-v4-flash".into(),
                token_usage: Some(TokenUsageLabel {
                    used: "21.8k".into(),
                    max: "1.0m".into(),
                }),
            }
        );
        assert_eq!(
            prompt_status_parts("local · deepseek/v4 · 512 / 128,000 · sync 50%"),
            PromptStatusParts {
                model_tag: "local · deepseek/v4 · sync 50%".into(),
                token_usage: Some(TokenUsageLabel {
                    used: "512".into(),
                    max: "128k".into(),
                }),
            }
        );
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

    fn assert_gauge_label_bg(backend: &TestBackend, label: &str, expected_bg: Color) {
        let area = backend.size().expect("backend size");
        let buffer = backend.buffer();
        let chars = label.chars().collect::<Vec<_>>();
        for y in 0..area.height {
            for x in 0..area.width.saturating_sub(chars.len() as u16) {
                let matches = chars
                    .iter()
                    .enumerate()
                    .all(|(offset, ch)| buffer[(x + offset as u16, y)].symbol() == ch.to_string());
                if matches {
                    for offset in 0..chars.len() {
                        assert_eq!(buffer[(x + offset as u16, y)].bg, expected_bg);
                    }
                    return;
                }
            }
        }
        panic!("label {label:?} not found");
    }
}
