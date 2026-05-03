use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::tui::theme::Palette;

/// Bottom ticker strip used in the trading floor view.
pub struct TickerWidget<'a> {
    pub cells: &'a [(String, String, Color)],
}

impl Widget for TickerWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let mut spans = vec![Span::styled(
            "[TICKER] ",
            Style::default()
                .fg(Palette::RED)
                .add_modifier(Modifier::BOLD),
        )];
        for (sub, msg, color) in self.cells {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("█", Style::default().fg(*color)));
            spans.push(Span::styled(
                format!(" #{sub} "),
                Style::default().add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw(format!("{msg} ")));
            spans.push(Span::styled("│", Style::default().fg(Palette::MUTED)));
        }
        Paragraph::new(Line::from(spans)).render(area, buf);
    }
}
