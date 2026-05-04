use ratatui::text::Line;

use super::{
    render_ir::{MarkdownParser, PulldownMarkdownParser, render_tui},
    theme::Theme,
};

pub fn render_markdown(text: &str, theme: &Theme) -> Vec<Line<'static>> {
    let document = PulldownMarkdownParser.parse(text);
    render_tui(&document, theme)
}
