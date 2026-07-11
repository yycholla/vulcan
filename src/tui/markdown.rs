use ratatui::{
    style::{Color, Style},
    text::Line,
};
use ratatui_markdown::{
    markdown::MarkdownRenderer,
    theme::{CodeColors, Generation, RichTextTheme},
};

use super::theme::Theme;

pub fn render_markdown(text: &str, theme: &Theme, width: u16) -> Vec<Line<'static>> {
    let renderer = MarkdownRenderer::new(width.max(1) as usize);
    let blocks = renderer.parse(text);
    renderer.render(&blocks, &MarkdownTheme(theme))
}

struct MarkdownTheme<'a>(&'a Theme);

impl RichTextTheme for MarkdownTheme<'_> {
    fn generation(&self) -> Generation {
        Generation(1)
    }

    fn get_text_color(&self) -> Color {
        fg(self.0.assistant, self.0.body_fg)
    }

    fn get_muted_text_color(&self) -> Color {
        fg(self.0.muted, Color::DarkGray)
    }

    fn get_primary_color(&self) -> Color {
        fg(self.0.accent, self.0.body_fg)
    }

    fn get_popup_selected_background(&self) -> Color {
        self.0.body_bg
    }

    fn get_border_color(&self) -> Color {
        fg(self.0.border, Color::DarkGray)
    }

    fn get_focused_border_color(&self) -> Color {
        fg(self.0.accent, self.0.body_fg)
    }

    fn get_secondary_color(&self) -> Color {
        fg(self.0.list_marker, self.0.body_fg)
    }

    fn get_info_color(&self) -> Color {
        fg(self.0.link, self.0.body_fg)
    }

    fn get_json_key_color(&self) -> Color {
        fg(self.0.link, self.0.body_fg)
    }

    fn get_json_string_color(&self) -> Color {
        fg(self.0.success, Color::Green)
    }

    fn get_json_number_color(&self) -> Color {
        fg(self.0.tool_call, Color::Yellow)
    }

    fn get_json_bool_color(&self) -> Color {
        fg(self.0.accent, self.0.body_fg)
    }

    fn get_json_null_color(&self) -> Color {
        fg(self.0.muted, Color::DarkGray)
    }

    fn get_accent_yellow(&self) -> Color {
        fg(self.0.list_marker, Color::Yellow)
    }

    fn get_code_colors(&self) -> CodeColors {
        let code = fg(self.0.code_block, self.0.body_fg);
        CodeColors {
            comment: fg(self.0.muted, Color::DarkGray),
            keyword: fg(self.0.accent, code),
            string: fg(self.0.success, code),
            string_escape: fg(self.0.success, code),
            number: fg(self.0.tool_call, code),
            constant: fg(self.0.tool_call, code),
            function: fg(self.0.link, code),
            r#type: fg(self.0.link, code),
            variable: code,
            property: fg(self.0.link, code),
            operator: fg(self.0.accent, code),
            punctuation: fg(self.0.muted, code),
            attribute: fg(self.0.tool_call, code),
            tag: fg(self.0.link, code),
            label: fg(self.0.error, code),
            error: fg(self.0.error, Color::Red),
        }
    }
}

fn fg(style: Style, fallback: Color) -> Color {
    style.fg.unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: Vec<Line<'static>>) -> Vec<String> {
        lines
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn renders_mermaid_blocks_through_markdown_renderer() {
        let rendered = texts(render_markdown(
            "```mermaid\ngraph TD\n    A[Hello]-->B[World]\n```",
            &Theme::system(),
            80,
        ));

        assert!(
            rendered.iter().any(|line| line.starts_with("╭─ mermaid")),
            "missing rendered mermaid frame: {rendered:?}"
        );
        assert!(
            rendered.iter().any(|line| line.contains("Hello")),
            "missing rendered mermaid node: {rendered:?}"
        );
    }

    #[test]
    fn renders_regular_code_blocks_through_markdown_renderer() {
        let rendered = texts(render_markdown(
            "```rust\nfn main() {}\n```",
            &Theme::system(),
            80,
        ));

        assert!(
            rendered.iter().any(|line| line.starts_with("╭─ rust")),
            "missing rendered rust code frame: {rendered:?}"
        );
    }

    #[test]
    fn keeps_punctuation_tight_after_inline_code() {
        let rendered = texts(render_markdown(
            "The file is a one-line `main`.",
            &Theme::system(),
            80,
        ));

        assert!(
            rendered.iter().any(|line| line.contains("main.")),
            "punctuation drifted away from inline code: {rendered:?}"
        );
    }

    #[test]
    fn preserves_prose_around_mermaid_blocks() {
        let rendered = texts(render_markdown(
            "Before the diagram.\n\n```mermaid\ngraph TD\n    A[Hello]-->B[World]\n```\n\nAfter `main`.",
            &Theme::system(),
            80,
        ));

        assert!(
            rendered
                .iter()
                .any(|line| line.contains("Before the diagram.")),
            "missing leading prose: {rendered:?}"
        );
        assert!(
            rendered.iter().any(|line| line.starts_with("╭─ mermaid")),
            "missing rendered mermaid block: {rendered:?}"
        );
        assert!(
            rendered.iter().any(|line| line.contains("After main.")),
            "missing trailing prose: {rendered:?}"
        );
    }
}
