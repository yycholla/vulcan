use std::path::PathBuf;

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::theme::Theme;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RenderDocument {
    pub blocks: Vec<RenderBlock>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenderBlock {
    BlankLine,
    Paragraph(Vec<Inline>),
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    CodeBlock {
        lang: Option<String>,
        lines: Vec<String>,
    },
    Quote(Vec<RenderBlock>),
    List {
        ordered: bool,
        items: Vec<ListItem>,
    },
    Table {
        headers: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
    Math {
        display: bool,
        tex: String,
    },
    Typst {
        source: String,
    },
    Media {
        path: PathBuf,
        alt: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListItem {
    pub number: Option<String>,
    pub blocks: Vec<RenderBlock>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Inline {
    Text(String),
    Code(String),
    Emphasis(Vec<Inline>),
    Strong(Vec<Inline>),
    Strike(Vec<Inline>),
    Link { text: Vec<Inline>, target: String },
}

pub trait MarkdownParser {
    fn parse(&self, text: &str) -> RenderDocument;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LegacyMarkdownParser;

impl MarkdownParser for LegacyMarkdownParser {
    fn parse(&self, text: &str) -> RenderDocument {
        let mut blocks = Vec::new();
        let mut in_code_block = false;
        let mut code_lang: Option<String> = None;
        let mut code_block_content: Vec<String> = Vec::new();

        for raw_line in text.lines() {
            let trimmed_start = raw_line.trim_start();
            if let Some(info) = trimmed_start.strip_prefix("```") {
                if in_code_block {
                    blocks.push(RenderBlock::CodeBlock {
                        lang: code_lang.take(),
                        lines: code_block_content.clone(),
                    });
                    code_block_content.clear();
                    in_code_block = false;
                } else {
                    in_code_block = true;
                    code_lang = parse_code_fence_lang(info);
                    code_block_content.clear();
                }
                continue;
            }

            if in_code_block {
                code_block_content.push(raw_line.to_string());
                continue;
            }

            let line = raw_line.trim_end();

            if line.is_empty() {
                blocks.push(RenderBlock::BlankLine);
                continue;
            }

            if let Some(level) = heading_level(line) {
                let content = line.trim_start_matches('#').trim();
                blocks.push(RenderBlock::Heading {
                    level,
                    content: parse_inline(content),
                });
                continue;
            }

            if let Some(content) = line.strip_prefix("> ") {
                blocks.push(RenderBlock::Quote(vec![RenderBlock::Paragraph(
                    parse_inline(content),
                )]));
                continue;
            }
            if line == ">" {
                blocks.push(RenderBlock::Quote(Vec::new()));
                continue;
            }

            if let Some(content) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
                blocks.push(RenderBlock::List {
                    ordered: false,
                    items: vec![ListItem {
                        number: None,
                        blocks: vec![RenderBlock::Paragraph(parse_inline(content))],
                    }],
                });
                continue;
            }

            if let Some((num_str, content)) = strip_ordered_list_prefix(line) {
                blocks.push(RenderBlock::List {
                    ordered: true,
                    items: vec![ListItem {
                        number: Some(num_str.to_string()),
                        blocks: vec![RenderBlock::Paragraph(parse_inline(content))],
                    }],
                });
                continue;
            }

            let trimmed = line.trim();
            if trimmed == "---" || trimmed == "***" || trimmed == "___" {
                blocks.push(RenderBlock::Rule);
                continue;
            }

            blocks.push(RenderBlock::Paragraph(parse_inline(line)));
        }

        if in_code_block {
            blocks.push(RenderBlock::CodeBlock {
                lang: code_lang,
                lines: code_block_content,
            });
        }

        RenderDocument { blocks }
    }
}

pub fn render_tui(document: &RenderDocument, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for block in &document.blocks {
        render_block_tui(block, theme, &mut lines);
    }
    lines
}

fn render_block_tui(block: &RenderBlock, theme: &Theme, lines: &mut Vec<Line<'static>>) {
    match block {
        RenderBlock::BlankLine => lines.push(Line::from("")),
        RenderBlock::Paragraph(inlines) => lines.push(Line::from(render_inlines(inlines, theme))),
        RenderBlock::Heading { level, content } => {
            let heading_style = match level {
                1 => theme.heading_1,
                2 => theme.heading_2,
                3 => theme.heading_3,
                4 => theme.heading_4,
                5 => theme.heading_5,
                _ => theme.heading_6,
            };
            let marker = match level {
                1 => "# ",
                2 => "## ",
                3 => "### ",
                4 => "#### ",
                5 => "##### ",
                _ => "###### ",
            };
            let mut spans = vec![Span::styled(marker, heading_style)];
            spans.extend(render_inlines(content, theme));
            lines.push(Line::from(spans));
        }
        RenderBlock::CodeBlock { lines: code, .. } => render_code_block_tui(code, theme, lines),
        RenderBlock::Quote(blocks) => {
            if blocks.is_empty() {
                lines.push(Line::from(Span::styled("▎", theme.blockquote)));
            } else {
                for nested in blocks {
                    let mut nested_lines = Vec::new();
                    render_block_tui(nested, theme, &mut nested_lines);
                    for nested_line in nested_lines {
                        let mut spans = vec![Span::styled("▎ ", theme.blockquote)];
                        spans.extend(nested_line.spans);
                        lines.push(Line::from(spans).style(theme.blockquote));
                    }
                }
            }
        }
        RenderBlock::List { ordered, items } => {
            for item in items {
                let marker = if *ordered {
                    format!("{}. ", item.number.as_deref().unwrap_or("1"))
                } else {
                    "• ".to_string()
                };
                render_list_item_tui(&marker, item, theme, lines);
            }
        }
        RenderBlock::Rule => lines.push(Line::from(Span::styled(
            "─".repeat(50),
            theme.muted.add_modifier(Modifier::DIM),
        ))),
        RenderBlock::Table { .. }
        | RenderBlock::Math { .. }
        | RenderBlock::Typst { .. }
        | RenderBlock::Media { .. } => lines.push(Line::from(Span::styled(
            "[unsupported render block]",
            theme.muted.add_modifier(Modifier::DIM),
        ))),
    }
}

fn render_list_item_tui(
    marker: &str,
    item: &ListItem,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
) {
    let mut first = true;
    for block in &item.blocks {
        let mut item_lines = Vec::new();
        render_block_tui(block, theme, &mut item_lines);
        for line in item_lines {
            if first {
                let mut spans = vec![Span::styled(marker.to_string(), theme.list_marker)];
                spans.extend(line.spans);
                lines.push(Line::from(spans));
                first = false;
            } else {
                lines.push(line);
            }
        }
    }
}

fn render_code_block_tui(code: &[String], theme: &Theme, lines: &mut Vec<Line<'static>>) {
    if code.is_empty() {
        lines.push(Line::from(Span::styled(
            " ```",
            theme.code_block.add_modifier(Modifier::DIM),
        )));
        return;
    }

    for line in code {
        lines.push(Line::from(Span::styled(
            format!(" │{}", line),
            theme.code_block,
        )));
    }
}

fn render_inlines(inlines: &[Inline], theme: &Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for inline in inlines {
        render_inline(inline, theme, Style::default(), &mut spans);
    }
    spans
}

fn render_inline(inline: &Inline, theme: &Theme, inherited: Style, spans: &mut Vec<Span<'static>>) {
    match inline {
        Inline::Text(text) => spans.push(Span::styled(text.clone(), inherited)),
        Inline::Code(code) => spans.push(Span::styled(code.clone(), theme.inline_code)),
        Inline::Emphasis(children) => render_inline_children(
            children,
            theme,
            inherited.add_modifier(Modifier::ITALIC),
            spans,
        ),
        Inline::Strong(children) => render_inline_children(
            children,
            theme,
            inherited.add_modifier(Modifier::BOLD),
            spans,
        ),
        Inline::Strike(children) => {
            render_inline_children(children, theme, theme.strikethrough, spans)
        }
        Inline::Link { text, .. } => render_inline_children(text, theme, theme.link, spans),
    }
}

fn render_inline_children(
    children: &[Inline],
    theme: &Theme,
    style: Style,
    spans: &mut Vec<Span<'static>>,
) {
    for child in children {
        render_inline(child, theme, style, spans);
    }
}

fn parse_code_fence_lang(info: &str) -> Option<String> {
    let lang = info.trim();
    (!lang.is_empty()).then(|| lang.to_string())
}

fn heading_level(line: &str) -> Option<u8> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    let mut count = 0;
    for ch in trimmed.chars() {
        if ch == '#' {
            count += 1;
        } else if ch == ' ' {
            break;
        } else {
            return None;
        }
    }
    if (1..=6).contains(&count) {
        Some(count)
    } else {
        None
    }
}

fn strip_ordered_list_prefix(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && i < bytes.len() && bytes[i] == b'.' {
        let num_str = &line[..i];
        let rest = line[i + 1..].trim();
        Some((num_str, rest))
    } else {
        None
    }
}

fn parse_inline(text: &str) -> Vec<Inline> {
    let mut spans: Vec<Inline> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if chars[i] == '\\' && i + 1 < len {
            spans.push(Inline::Text(chars[i + 1].to_string()));
            i += 2;
            continue;
        }

        if chars[i] == '`' {
            let start = i + 1;
            if let Some(end) = chars[start..].iter().position(|&c| c == '`') {
                let code: String = chars[start..start + end].iter().collect();
                spans.push(Inline::Code(code));
                i = start + end + 1;
                continue;
            }
        }

        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            let start = i + 2;
            if let Some(end) = chars[start..].windows(2).position(|w| w == ['*', '*']) {
                let inner: String = chars[start..start + end].iter().collect();
                spans.push(Inline::Strong(parse_inline(&inner)));
                i = start + end + 2;
                continue;
            }
        }

        if chars[i] == '*' && (i + 1 >= len || chars[i + 1] != '*') {
            let start = i + 1;
            if let Some(end) = chars[start..].iter().position(|&c| c == '*') {
                let inner: String = chars[start..start + end].iter().collect();
                spans.push(Inline::Emphasis(vec![Inline::Text(inner)]));
                i = start + end + 1;
                continue;
            }
        }

        if i + 1 < len && chars[i] == '~' && chars[i + 1] == '~' {
            let start = i + 2;
            if let Some(end) = chars[start..].windows(2).position(|w| w == ['~', '~']) {
                let inner: String = chars[start..start + end].iter().collect();
                spans.push(Inline::Strike(vec![Inline::Text(inner)]));
                i = start + end + 2;
                continue;
            }
        }

        if chars[i] == '[' {
            let start = i + 1;
            if let Some(close_bracket) = chars[start..].iter().position(|&c| c == ']') {
                let text_inner: String = chars[start..start + close_bracket].iter().collect();
                let after_close = start + close_bracket + 1;
                if after_close < len && chars[after_close] == '(' {
                    let url_start = after_close + 1;
                    if let Some(close_paren) = chars[url_start..].iter().position(|&c| c == ')') {
                        let target: String =
                            chars[url_start..url_start + close_paren].iter().collect();
                        spans.push(Inline::Link {
                            text: parse_inline(&text_inner),
                            target,
                        });
                        i = url_start + close_paren + 1;
                        continue;
                    }
                }
            }
        }

        spans.push(Inline::Text(chars[i].to_string()));
        i += 1;
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> RenderDocument {
        LegacyMarkdownParser.parse(text)
    }

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn parses_representative_markdown_blocks_to_ir() {
        let doc = parse("# Title\n\n> quote\n- item\n12. step\n---\n```\ncode\n```");

        assert_eq!(doc.blocks.len(), 7);
        assert!(matches!(
            doc.blocks[0],
            RenderBlock::Heading { level: 1, .. }
        ));
        assert_eq!(doc.blocks[1], RenderBlock::BlankLine);
        assert!(matches!(doc.blocks[2], RenderBlock::Quote(_)));
        assert!(matches!(
            doc.blocks[3],
            RenderBlock::List { ordered: false, .. }
        ));
        assert!(matches!(
            doc.blocks[4],
            RenderBlock::List { ordered: true, .. }
        ));
        assert_eq!(doc.blocks[5], RenderBlock::Rule);
        assert!(matches!(doc.blocks[6], RenderBlock::CodeBlock { .. }));
    }

    #[test]
    fn parses_inline_ir_for_current_markdown_subset() {
        let doc = parse("hello `code` **bold** *em* ~~gone~~ [link](https://example.com)");

        let RenderBlock::Paragraph(inlines) = &doc.blocks[0] else {
            panic!("expected paragraph");
        };
        assert!(
            inlines
                .iter()
                .any(|inline| matches!(inline, Inline::Code(text) if text == "code"))
        );
        assert!(
            inlines
                .iter()
                .any(|inline| matches!(inline, Inline::Strong(_)))
        );
        assert!(
            inlines
                .iter()
                .any(|inline| matches!(inline, Inline::Emphasis(_)))
        );
        assert!(
            inlines
                .iter()
                .any(|inline| matches!(inline, Inline::Strike(_)))
        );
        assert!(inlines.iter().any(|inline| {
            matches!(inline, Inline::Link { target, .. } if target == "https://example.com")
        }));
    }

    #[test]
    fn renders_existing_subset_from_ir_to_tui_lines() {
        let theme = Theme::system();
        let doc = parse("# Title\n> quote\n- item\n```\ncode\n```");
        let rendered: Vec<String> = render_tui(&doc, &theme).iter().map(line_text).collect();

        assert_eq!(rendered, vec!["# Title", "▎ quote", "• item", " │code"]);
    }

    #[test]
    fn code_block_ir_preserves_blank_code_rows() {
        let theme = Theme::system();
        let doc = parse("```\nfirst\n\n```");
        let rendered: Vec<String> = render_tui(&doc, &theme).iter().map(line_text).collect();

        assert_eq!(rendered, vec![" │first", " │"]);
    }

    #[test]
    fn ir_includes_future_renderer_variants_without_heavy_dependencies() {
        let doc = RenderDocument {
            blocks: vec![
                RenderBlock::Table {
                    headers: vec![vec![Inline::Text("h".into())]],
                    rows: vec![vec![vec![Inline::Text("r".into())]]],
                },
                RenderBlock::Math {
                    display: true,
                    tex: "x^2".into(),
                },
                RenderBlock::Typst {
                    source: "#set text".into(),
                },
                RenderBlock::Media {
                    path: PathBuf::from("plot.png"),
                    alt: "plot".into(),
                },
            ],
        };

        assert_eq!(doc.blocks.len(), 4);
    }
}
