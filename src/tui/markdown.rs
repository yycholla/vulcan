use ratatui::text::Line;

use super::{
    render_ir::{LegacyMarkdownParser, MarkdownParser, render_tui},
    theme::Theme,
};

pub fn render_markdown(text: &str, theme: &Theme) -> Vec<Line<'static>> {
    let document = LegacyMarkdownParser.parse(text);
    render_tui(&document, theme)
}
