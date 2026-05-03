use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::tui::{
    picker_state::ProviderPickerEntry,
    surface::{SurfacePlacement, resolve_surface_area},
    theme::Theme,
};

pub struct ProviderPickerWidget<'a> {
    pub theme: &'a Theme,
    pub items: &'a [ProviderPickerEntry],
    pub selection: usize,
}

impl Widget for ProviderPickerWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let width = area.width.min(72);
        let rows = (self.items.len() as u16).min(12);
        let height = (rows + 5).min(area.height.saturating_sub(2));
        let box_area = resolve_surface_area(area, SurfacePlacement::Modal { width, height });
        if box_area.height < 4 {
            return;
        }
        fill_rect(buf, box_area, Style::default().bg(self.theme.body_bg));

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
        Paragraph::new(title)
            .style(
                self.theme
                    .accent
                    .add_modifier(Modifier::BOLD)
                    .bg(self.theme.body_bg),
            )
            .render(bar, buf);

        let list_area = Rect {
            x: box_area.x,
            y: box_area.y + 1,
            width: box_area.width,
            height: box_area.height.saturating_sub(2),
        };

        let mut lines: Vec<Line<'static>> = Vec::new();
        if self.items.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (no providers)",
                self.theme.muted,
            )));
        } else {
            let active = self.selection.min(self.items.len().saturating_sub(1));
            for (i, e) in self.items.iter().enumerate() {
                let is_active = i == active;
                let marker = if is_active { "▸ " } else { "  " };
                let label = e.name.clone().unwrap_or_else(|| "default".into());
                let row_style =
                    Style::default()
                        .fg(self.theme.body_fg)
                        .add_modifier(if is_active {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        });
                lines.push(Line::from(vec![
                    Span::styled(marker, row_style.add_modifier(Modifier::BOLD)),
                    Span::styled(format!("{label:<12}"), row_style),
                    Span::styled(format!(" {}", e.model), self.theme.muted),
                    Span::styled(format!("  ({})", e.base_url), self.theme.muted),
                ]));
            }
        }

        let hint = "  ↑↓ navigate · Enter select · Esc cancel  ";
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(hint, self.theme.muted)));

        Paragraph::new(lines)
            .style(Style::default().bg(self.theme.body_bg))
            .render(list_area, buf);
        draw_picker_border(buf, box_area, self.theme);
    }
}

fn fill_rect(buf: &mut Buffer, r: Rect, style: Style) {
    for y in r.y..r.bottom() {
        for x in r.x..r.right() {
            buf[(x, y)].set_symbol(" ").set_style(style);
        }
    }
}

fn draw_picker_border(buf: &mut Buffer, r: Rect, theme: &Theme) {
    let style = theme.border;
    if r.width == 0 || r.height == 0 {
        return;
    }

    for x in r.x..r.x + r.width {
        buf[(x, r.y)].set_symbol("─").set_style(style);
        if r.height > 1 {
            buf[(x, r.y + r.height - 1)]
                .set_symbol("─")
                .set_style(style);
        }
    }

    if r.height > 2 {
        for y in r.y + 1..r.y + r.height - 1 {
            buf[(r.x, y)].set_symbol("│").set_style(style);
            if r.width > 1 {
                buf[(r.x + r.width - 1, y)].set_symbol("│").set_style(style);
            }
        }
    }
}
