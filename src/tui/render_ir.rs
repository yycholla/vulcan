use std::path::PathBuf;

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::theme::Theme;

// Renderer parser decision record (GH #585):
// use pulldown-cmark's low-overhead event stream as Vulcan's first real
// parser behind the owned Render IR. Avoid tui-markdown as the production path
// because chat_render must keep control of accent rails, wrapping, caching, and
// theme roles. Keep comrak deferred until Vulcan needs AST-heavy transforms.

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

#[derive(Clone, Copy, Debug, Default)]
pub struct PulldownMarkdownParser;

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

impl MarkdownParser for PulldownMarkdownParser {
    fn parse(&self, text: &str) -> RenderDocument {
        parse_pulldown(text)
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
        RenderBlock::CodeBlock { lang, lines: code } => {
            render_code_block_tui(lang.as_deref(), code, theme, lines)
        }
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
                let (marker, strip_task_marker) = if *ordered {
                    (
                        format!("{}. ", item.number.as_deref().unwrap_or("1")),
                        false,
                    )
                } else if let Some(marker) = task_list_marker(item) {
                    (marker.to_string(), true)
                } else {
                    ("• ".to_string(), false)
                };
                render_list_item_tui(&marker, item, theme, lines, strip_task_marker);
            }
        }
        RenderBlock::Rule => lines.push(Line::from(Span::styled(
            "─".repeat(50),
            theme.muted.add_modifier(Modifier::DIM),
        ))),
        RenderBlock::Table { headers, rows } => render_table_tui(headers, rows, theme, lines),
        RenderBlock::Math { .. } | RenderBlock::Typst { .. } | RenderBlock::Media { .. } => lines
            .push(Line::from(Span::styled(
                "[unsupported render block]",
                theme.muted.add_modifier(Modifier::DIM),
            ))),
    }
}

fn render_table_tui(
    headers: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
) {
    if headers.is_empty() && rows.is_empty() {
        return;
    }
    if !headers.is_empty() {
        let widths = table_column_widths(headers, rows);
        lines.push(render_table_row(headers, &widths, theme));
        lines.push(render_table_separator(&widths, theme));
        for row in rows {
            lines.push(render_table_row(row, &widths, theme));
        }
        return;
    }
    let widths = table_column_widths(&[], rows);
    for row in rows {
        lines.push(render_table_row(row, &widths, theme));
    }
}

fn table_column_widths(headers: &[Vec<Inline>], rows: &[Vec<Vec<Inline>>]) -> Vec<usize> {
    const MAX_CELL_WIDTH: usize = 32;

    let columns = headers
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    let mut widths = vec![3usize; columns];
    for (idx, cell) in headers.iter().enumerate() {
        widths[idx] = widths[idx].max(display_width(&flatten_inlines(cell)).min(MAX_CELL_WIDTH));
    }
    for row in rows {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] =
                widths[idx].max(display_width(&flatten_inlines(cell)).min(MAX_CELL_WIDTH));
        }
    }
    widths
}

fn render_table_row(cells: &[Vec<Inline>], widths: &[usize], theme: &Theme) -> Line<'static> {
    let mut spans = Vec::new();
    spans.push(Span::styled("|", theme.muted));
    for (idx, width) in widths.iter().enumerate() {
        let cell = cells
            .get(idx)
            .map(|cell| flatten_inlines(cell))
            .unwrap_or_default();
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            pad_cell(&cell, *width),
            Style::default().fg(theme.body_fg),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("|", theme.muted));
    }
    Line::from(spans)
}

fn render_table_separator(widths: &[usize], theme: &Theme) -> Line<'static> {
    let mut spans = Vec::new();
    spans.push(Span::styled("|", theme.muted));
    for width in widths {
        spans.push(Span::styled(
            format!(" {} ", "-".repeat(*width)),
            theme.muted.add_modifier(Modifier::DIM),
        ));
        spans.push(Span::styled("|", theme.muted));
    }
    Line::from(spans)
}

fn pad_cell(cell: &str, width: usize) -> String {
    let value = truncate_display(cell, width);
    let padding = width.saturating_sub(display_width(&value));
    format!("{value}{}", " ".repeat(padding))
}

fn truncate_display(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let width = ch.width().unwrap_or(0);
        if used + width > max_width - 3 {
            break;
        }
        out.push(ch);
        used += width;
    }
    out.push_str("...");
    out
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn task_list_marker(item: &ListItem) -> Option<&'static str> {
    let first = item.blocks.first()?;
    let RenderBlock::Paragraph(inlines) = first else {
        return None;
    };
    let first_text = match inlines.first()? {
        Inline::Text(text) => text,
        _ => return None,
    };
    if first_text.starts_with("[x] ") || first_text.starts_with("[X] ") {
        Some("☑ ")
    } else if first_text.starts_with("[ ] ") {
        Some("☐ ")
    } else {
        None
    }
}

fn strip_task_list_prefix(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut spans = spans;
    if let Some(first) = spans.first_mut() {
        for prefix in ["[x] ", "[X] ", "[ ] "] {
            if let Some(rest) = first.content.as_ref().strip_prefix(prefix) {
                first.content = rest.to_string().into();
                break;
            }
        }
    }
    if spans
        .first()
        .is_some_and(|span| span.content.as_ref().is_empty())
    {
        spans.remove(0);
    }
    spans
}

fn render_list_item_tui(
    marker: &str,
    item: &ListItem,
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
    strip_task_marker: bool,
) {
    let mut first = true;
    for block in &item.blocks {
        let mut item_lines = Vec::new();
        render_block_tui(block, theme, &mut item_lines);
        for line in item_lines {
            if first {
                let mut spans = vec![Span::styled(marker.to_string(), theme.list_marker)];
                let item_spans = if strip_task_marker {
                    strip_task_list_prefix(line.spans)
                } else {
                    line.spans
                };
                spans.extend(item_spans);
                lines.push(Line::from(spans));
                first = false;
            } else {
                let mut spans = vec![Span::raw(" ".repeat(display_width(marker)))];
                spans.extend(line.spans);
                lines.push(Line::from(spans));
            }
        }
    }
}

fn render_code_block_tui(
    lang: Option<&str>,
    code: &[String],
    theme: &Theme,
    lines: &mut Vec<Line<'static>>,
) {
    if let Some(lang) = lang.filter(|lang| !lang.is_empty()) {
        lines.push(Line::from(Span::styled(
            format!(" ```{lang}"),
            theme.code_block.add_modifier(Modifier::DIM),
        )));
    }
    if code.is_empty() {
        lines.push(Line::from(Span::styled(
            " ```",
            theme.code_block.add_modifier(Modifier::DIM),
        )));
        return;
    }

    for line in code {
        let mut spans = vec![Span::styled(" │", theme.code_block)];
        spans.extend(highlight_code_line(lang, line, theme));
        lines.push(Line::from(spans));
    }
}

fn highlight_code_line(lang: Option<&str>, line: &str, theme: &Theme) -> Vec<Span<'static>> {
    let Some(lang) = lang
        .map(normalize_code_lang)
        .filter(|lang| !lang.is_empty())
    else {
        return vec![Span::styled(line.to_string(), theme.code_block)];
    };
    if !matches!(lang, "rust" | "toml" | "json") {
        return vec![Span::styled(line.to_string(), theme.code_block)];
    }

    let mut spans = Vec::new();
    let mut chars = line.char_indices().peekable();
    let mut expect_toml_key = lang == "toml";
    while let Some((start, ch)) = chars.next() {
        if lang == "rust" && line[start..].starts_with("//") {
            spans.push(Span::styled(line[start..].to_string(), theme.muted));
            break;
        }
        if lang == "toml" && ch == '#' {
            spans.push(Span::styled(line[start..].to_string(), theme.muted));
            break;
        }
        if ch == '"' {
            let end = consume_quoted_string(line, &mut chars);
            spans.push(Span::styled(
                line[start..end].to_string(),
                theme.inline_code,
            ));
            continue;
        }
        if ch.is_ascii_digit()
            || (ch == '-' && chars.peek().is_some_and(|(_, next)| next.is_ascii_digit()))
        {
            let end = consume_while(line, &mut chars, |next| {
                next.is_ascii_alphanumeric() || matches!(next, '.' | '_' | '-')
            });
            spans.push(Span::styled(line[start..end].to_string(), theme.accent));
            continue;
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let end = consume_while(line, &mut chars, |next| {
                next.is_ascii_alphanumeric() || next == '_'
            });
            let token = &line[start..end];
            let style = if lang == "rust" && is_rust_keyword(token) {
                theme.link.add_modifier(Modifier::BOLD)
            } else if lang == "json" && matches!(token, "true" | "false" | "null") {
                theme.accent
            } else if lang == "toml" && expect_toml_key {
                theme.link.add_modifier(Modifier::BOLD)
            } else {
                theme.code_block
            };
            spans.push(Span::styled(token.to_string(), style));
            expect_toml_key = false;
            continue;
        }
        if lang == "toml" && ch == '=' {
            expect_toml_key = false;
        }
        spans.push(Span::styled(ch.to_string(), theme.code_block));
    }
    spans
}

fn normalize_code_lang(lang: &str) -> &str {
    match lang.trim().to_ascii_lowercase().as_str() {
        "rs" | "rust" => "rust",
        "toml" => "toml",
        "json" | "jsonc" => "json",
        _ => "",
    }
}

fn consume_quoted_string<I>(line: &str, chars: &mut std::iter::Peekable<I>) -> usize
where
    I: Iterator<Item = (usize, char)>,
{
    let mut escaped = false;
    for (idx, ch) in chars.by_ref() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return idx + ch.len_utf8();
        }
    }
    line.len()
}

fn consume_while<I>(
    line: &str,
    chars: &mut std::iter::Peekable<I>,
    mut keep: impl FnMut(char) -> bool,
) -> usize
where
    I: Iterator<Item = (usize, char)>,
{
    while let Some((_, ch)) = chars.peek() {
        if !keep(*ch) {
            break;
        }
        chars.next();
    }
    chars.peek().map(|(idx, _)| *idx).unwrap_or(line.len())
}

fn is_rust_keyword(token: &str) -> bool {
    matches!(
        token,
        "as" | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "else"
            | "enum"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
    )
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

fn parse_pulldown(text: &str) -> RenderDocument {
    use pulldown_cmark::{Options, Parser};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let mut events = Parser::new_ext(text, options).peekable();
    RenderDocument {
        blocks: parse_pulldown_blocks(&mut events, None),
    }
}

fn parse_pulldown_blocks<'a, I>(
    events: &mut std::iter::Peekable<I>,
    end: Option<pulldown_cmark::TagEnd>,
) -> Vec<RenderBlock>
where
    I: Iterator<Item = pulldown_cmark::Event<'a>>,
{
    use pulldown_cmark::{Event, Tag, TagEnd};

    let mut blocks = Vec::new();
    while let Some(event) = events.next() {
        match event {
            Event::End(tag_end) if Some(tag_end) == end => break,
            Event::Start(Tag::Paragraph) => {
                blocks.push(RenderBlock::Paragraph(parse_pulldown_inlines(
                    events,
                    TagEnd::Paragraph,
                )));
            }
            Event::Start(Tag::Heading { level, .. }) => {
                blocks.push(RenderBlock::Heading {
                    level: heading_level_to_u8(level),
                    content: parse_pulldown_inlines(events, TagEnd::Heading(level)),
                });
            }
            Event::Start(Tag::BlockQuote(kind)) => {
                blocks.push(RenderBlock::Quote(parse_pulldown_blocks(
                    events,
                    Some(TagEnd::BlockQuote(kind)),
                )));
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                blocks.push(parse_pulldown_code_block(events, kind));
            }
            Event::Start(Tag::List(first_number)) => {
                blocks.push(parse_pulldown_list(events, first_number));
            }
            Event::Start(Tag::Table(_)) => {
                blocks.push(parse_pulldown_table(events));
            }
            Event::Rule => blocks.push(RenderBlock::Rule),
            Event::TaskListMarker(checked) => {
                let mut inlines = vec![Inline::Text(
                    if checked { "[x] " } else { "[ ] " }.to_string(),
                )];
                collect_task_list_item_inlines(events, end, &mut inlines);
                blocks.push(RenderBlock::Paragraph(inlines));
                if end == Some(TagEnd::Item) {
                    return blocks;
                }
            }
            Event::DisplayMath(tex) => blocks.push(RenderBlock::Math {
                display: true,
                tex: tex.to_string(),
            }),
            Event::Text(text) if !text.is_empty() => {
                blocks.push(RenderBlock::Paragraph(vec![Inline::Text(text.to_string())]));
            }
            Event::Code(code) => {
                blocks.push(RenderBlock::Paragraph(vec![Inline::Code(code.to_string())]))
            }
            Event::SoftBreak | Event::HardBreak => blocks.push(RenderBlock::BlankLine),
            Event::Html(html) | Event::InlineHtml(html) => {
                blocks.push(RenderBlock::Paragraph(vec![Inline::Text(html.to_string())]));
            }
            Event::End(_) => {}
            _ => {}
        }
    }
    blocks
}

fn collect_task_list_item_inlines<'a, I>(
    events: &mut std::iter::Peekable<I>,
    block_end: Option<pulldown_cmark::TagEnd>,
    inlines: &mut Vec<Inline>,
) where
    I: Iterator<Item = pulldown_cmark::Event<'a>>,
{
    use pulldown_cmark::{Event, Tag, TagEnd};

    while let Some(event) = events.next() {
        match event {
            Event::End(tag_end) if Some(tag_end) == block_end => break,
            Event::End(TagEnd::Paragraph) => break,
            Event::Text(text) => inlines.push(Inline::Text(text.to_string())),
            Event::Code(code) => inlines.push(Inline::Code(code.to_string())),
            Event::SoftBreak | Event::HardBreak => inlines.push(Inline::Text(" ".to_string())),
            Event::Html(html) | Event::InlineHtml(html) => {
                inlines.push(Inline::Text(html.to_string()));
            }
            Event::Start(Tag::Emphasis) => {
                inlines.push(Inline::Emphasis(parse_pulldown_inlines(
                    events,
                    TagEnd::Emphasis,
                )));
            }
            Event::Start(Tag::Strong) => {
                inlines.push(Inline::Strong(parse_pulldown_inlines(
                    events,
                    TagEnd::Strong,
                )));
            }
            Event::Start(Tag::Strikethrough) => {
                inlines.push(Inline::Strike(parse_pulldown_inlines(
                    events,
                    TagEnd::Strikethrough,
                )));
            }
            Event::Start(Tag::Link {
                dest_url, title: _, ..
            }) => {
                inlines.push(Inline::Link {
                    text: parse_pulldown_inlines(events, TagEnd::Link),
                    target: dest_url.to_string(),
                });
            }
            _ => {}
        }
    }
}

fn parse_pulldown_inlines<'a, I>(
    events: &mut std::iter::Peekable<I>,
    end: pulldown_cmark::TagEnd,
) -> Vec<Inline>
where
    I: Iterator<Item = pulldown_cmark::Event<'a>>,
{
    use pulldown_cmark::{Event, Tag, TagEnd};

    let mut inlines = Vec::new();
    while let Some(event) = events.next() {
        match event {
            Event::End(tag_end) if tag_end == end => break,
            Event::Text(text) => inlines.push(Inline::Text(text.to_string())),
            Event::Code(code) => inlines.push(Inline::Code(code.to_string())),
            Event::InlineMath(tex) => inlines.push(Inline::Text(format!("${tex}$"))),
            Event::DisplayMath(tex) => inlines.push(Inline::Text(format!("$${tex}$$"))),
            Event::SoftBreak | Event::HardBreak => inlines.push(Inline::Text(" ".to_string())),
            Event::Html(html) | Event::InlineHtml(html) => {
                inlines.push(Inline::Text(html.to_string()));
            }
            Event::TaskListMarker(checked) => {
                inlines.push(Inline::Text(
                    if checked { "[x] " } else { "[ ] " }.to_string(),
                ));
            }
            Event::Start(Tag::Emphasis) => {
                inlines.push(Inline::Emphasis(parse_pulldown_inlines(
                    events,
                    TagEnd::Emphasis,
                )));
            }
            Event::Start(Tag::Strong) => {
                inlines.push(Inline::Strong(parse_pulldown_inlines(
                    events,
                    TagEnd::Strong,
                )));
            }
            Event::Start(Tag::Strikethrough) => {
                inlines.push(Inline::Strike(parse_pulldown_inlines(
                    events,
                    TagEnd::Strikethrough,
                )));
            }
            Event::Start(Tag::Link {
                dest_url, title: _, ..
            }) => {
                inlines.push(Inline::Link {
                    text: parse_pulldown_inlines(events, TagEnd::Link),
                    target: dest_url.to_string(),
                });
            }
            Event::Start(Tag::Image {
                dest_url, title: _, ..
            }) => {
                let alt = flatten_inlines(&parse_pulldown_inlines(events, TagEnd::Image));
                inlines.push(Inline::Text(format!("![{alt}]({dest_url})")));
            }
            Event::Start(_) => {}
            Event::End(_) => {}
            Event::Rule | Event::FootnoteReference(_) => {}
        }
    }
    inlines
}

fn parse_pulldown_code_block<'a, I>(
    events: &mut std::iter::Peekable<I>,
    kind: pulldown_cmark::CodeBlockKind<'a>,
) -> RenderBlock
where
    I: Iterator<Item = pulldown_cmark::Event<'a>>,
{
    use pulldown_cmark::{CodeBlockKind, Event, TagEnd};

    let lang = match kind {
        CodeBlockKind::Fenced(info) => info
            .split_whitespace()
            .next()
            .filter(|lang| !lang.is_empty())
            .map(str::to_string),
        CodeBlockKind::Indented => None,
    };
    let mut code = String::new();
    for event in events.by_ref() {
        match event {
            Event::End(TagEnd::CodeBlock) => break,
            Event::Text(text) | Event::Code(text) => code.push_str(&text),
            Event::SoftBreak | Event::HardBreak => code.push('\n'),
            _ => {}
        }
    }
    RenderBlock::CodeBlock {
        lang,
        lines: code_to_lines(&code),
    }
}

fn parse_pulldown_list<'a, I>(
    events: &mut std::iter::Peekable<I>,
    first_number: Option<u64>,
) -> RenderBlock
where
    I: Iterator<Item = pulldown_cmark::Event<'a>>,
{
    use pulldown_cmark::{Event, Tag, TagEnd};

    let ordered = first_number.is_some();
    let mut next_number = first_number.unwrap_or(1);
    let mut items = Vec::new();
    while let Some(event) = events.next() {
        match event {
            Event::End(TagEnd::List(_)) => break,
            Event::Start(Tag::Item) => {
                let number = ordered.then(|| {
                    let current = next_number.to_string();
                    next_number = next_number.saturating_add(1);
                    current
                });
                items.push(ListItem {
                    number,
                    blocks: parse_pulldown_blocks(events, Some(TagEnd::Item)),
                });
            }
            _ => {}
        }
    }

    RenderBlock::List { ordered, items }
}

fn parse_pulldown_table<'a, I>(events: &mut std::iter::Peekable<I>) -> RenderBlock
where
    I: Iterator<Item = pulldown_cmark::Event<'a>>,
{
    use pulldown_cmark::{Event, Tag, TagEnd};

    let mut headers = Vec::new();
    let mut rows = Vec::new();
    let mut in_head = false;
    let mut current_row: Vec<Vec<Inline>> = Vec::new();

    while let Some(event) = events.next() {
        match event {
            Event::End(TagEnd::Table) => break,
            Event::Start(Tag::TableHead) => in_head = true,
            Event::End(TagEnd::TableHead) => {
                if !current_row.is_empty() {
                    headers = current_row.clone();
                    current_row.clear();
                }
                in_head = false;
            }
            Event::Start(Tag::TableRow) => current_row.clear(),
            Event::End(TagEnd::TableRow) => {
                if in_head {
                    headers = current_row.clone();
                } else {
                    rows.push(current_row.clone());
                }
            }
            Event::Start(Tag::TableCell) => {
                current_row.push(parse_pulldown_inlines(events, TagEnd::TableCell));
            }
            _ => {}
        }
    }

    RenderBlock::Table { headers, rows }
}

fn code_to_lines(code: &str) -> Vec<String> {
    if code.is_empty() {
        return Vec::new();
    }
    let mut lines = code.split('\n').map(str::to_string).collect::<Vec<_>>();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines
}

fn heading_level_to_u8(level: pulldown_cmark::HeadingLevel) -> u8 {
    match level {
        pulldown_cmark::HeadingLevel::H1 => 1,
        pulldown_cmark::HeadingLevel::H2 => 2,
        pulldown_cmark::HeadingLevel::H3 => 3,
        pulldown_cmark::HeadingLevel::H4 => 4,
        pulldown_cmark::HeadingLevel::H5 => 5,
        pulldown_cmark::HeadingLevel::H6 => 6,
    }
}

fn flatten_inlines(inlines: &[Inline]) -> String {
    let mut text = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(value) | Inline::Code(value) => text.push_str(value),
            Inline::Emphasis(children) | Inline::Strong(children) | Inline::Strike(children) => {
                text.push_str(&flatten_inlines(children));
            }
            Inline::Link { text: children, .. } => text.push_str(&flatten_inlines(children)),
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> RenderDocument {
        LegacyMarkdownParser.parse(text)
    }

    fn parse_commonmark(text: &str) -> RenderDocument {
        PulldownMarkdownParser.parse(text)
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

    #[test]
    fn pulldown_parser_maps_commonmark_blocks_to_ir() {
        let doc = parse_commonmark(
            "# Title\n\n> quote\n\n1. first\n2. second\n\n```rust\nfn main() {}\n```",
        );

        assert!(matches!(
            doc.blocks[0],
            RenderBlock::Heading { level: 1, .. }
        ));
        assert!(matches!(doc.blocks[1], RenderBlock::Quote(_)));
        assert!(matches!(
            doc.blocks[2],
            RenderBlock::List { ordered: true, .. }
        ));
        assert!(matches!(
            doc.blocks[3],
            RenderBlock::CodeBlock {
                lang: Some(ref lang),
                ..
            } if lang == "rust"
        ));
    }

    #[test]
    fn pulldown_parser_maps_gfm_tables_and_task_lists() {
        let doc = parse_commonmark("- [x] done\n- [ ] todo\n\n| a | b |\n|---|---|\n| c | d |");

        let RenderBlock::List { items, .. } = &doc.blocks[0] else {
            panic!("expected task list");
        };
        let first = flatten_inlines(match &items[0].blocks[0] {
            RenderBlock::Paragraph(inlines) => inlines,
            _ => panic!("expected task item paragraph"),
        });
        assert_eq!(first, "[x] done");

        let RenderBlock::Table { headers, rows } = &doc.blocks[1] else {
            panic!("expected table");
        };
        assert_eq!(headers.len(), 2);
        assert_eq!(rows.len(), 1);
        assert_eq!(flatten_inlines(&rows[0][1]), "d");
    }

    #[test]
    fn pulldown_renderer_outputs_aligned_table_rows() {
        let theme = Theme::system();
        let doc = parse_commonmark("| a | b |\n|---|---|\n| c | d |");
        let rendered = render_tui(&doc, &theme)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>();

        assert_eq!(
            rendered,
            vec!["| a   | b   |", "| --- | --- |", "| c   | d   |"]
        );
    }

    #[test]
    fn table_renderer_caps_long_cells_for_narrow_fallback() {
        let theme = Theme::system();
        let doc = parse_commonmark(
            "| column | value |\n|---|---|\n| a very long value that should cap | ok |",
        );
        let rendered = render_tui(&doc, &theme)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>();

        assert!(rendered[2].contains("a very long value that should..."));
    }

    #[test]
    fn task_lists_render_checkbox_markers() {
        let theme = Theme::system();
        let doc = parse_commonmark("- [x] done\n- [ ] todo");
        let rendered = render_tui(&doc, &theme)
            .iter()
            .map(line_text)
            .collect::<Vec<_>>();

        assert_eq!(rendered, vec!["☑ done", "☐ todo"]);
    }

    #[test]
    fn code_fences_render_language_header_and_highlight_tokens() {
        let theme = Theme::system();
        let doc = parse_commonmark("```rust\nfn main() { let answer = 42; }\n```");
        let rendered = render_tui(&doc, &theme);

        assert_eq!(line_text(&rendered[0]), " ```rust");
        assert_eq!(line_text(&rendered[1]), " │fn main() { let answer = 42; }");
        assert!(
            rendered[1]
                .spans
                .iter()
                .any(|span| span.style == theme.link.add_modifier(Modifier::BOLD)
                    && span.content == "fn")
        );
        assert!(
            rendered[1]
                .spans
                .iter()
                .any(|span| span.style == theme.accent && span.content == "42")
        );
    }

    #[test]
    fn unknown_code_fence_languages_fall_back_to_plain_code_style() {
        let theme = Theme::system();
        let doc = parse_commonmark("```unknown\nsome words 123\n```");
        let rendered = render_tui(&doc, &theme);

        assert_eq!(line_text(&rendered[1]), " │some words 123");
        assert_eq!(rendered[1].spans[1].style, theme.code_block);
    }

    #[test]
    fn legacy_parser_remains_available_as_fallback() {
        let legacy = LegacyMarkdownParser.parse("[not a link]");
        let pulldown = PulldownMarkdownParser.parse("[not a link]");

        assert_eq!(render_tui(&legacy, &Theme::system()).len(), 1);
        assert_eq!(render_tui(&pulldown, &Theme::system()).len(), 1);
    }
}
