use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use super::theme::Palette;

struct MdTheme;

impl MdTheme {
    const HEADING: Color = Color::Rgb(0xD6, 0x3B, 0x2F); // red accent
    const ITALIC: Color = Color::Rgb(0x2B, 0x4F, 0xA8); // blue accent
    const CODE: Color = Color::Rgb(0xE8, 0xB4, 0x3C); // yellow accent
    const LINK: Color = Color::Rgb(0x2B, 0x4F, 0xA8);
    const STRIKE: Color = Palette::MUTED;
    const QUOTE: Color = Palette::MUTED;
    const LIST_BULLET: Color = Color::Rgb(0xD6, 0x3B, 0x2F);
    const HR: Color = Palette::MUTED;
    const CODE_BLOCK_BG: Color = Color::Rgb(0x22, 0x20, 0x1B);
}

pub fn render_markdown(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code_block = false;
    let mut code_block_content: Vec<String> = Vec::new();

    for raw_line in text.lines() {
        if raw_line.trim_start().starts_with("```") {
            if in_code_block {
                let block_lines = render_code_block(&code_block_content);
                lines.extend(block_lines);
                code_block_content.clear();
                in_code_block = false;
            } else {
                in_code_block = true;
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
            lines.push(Line::from(""));
            continue;
        }

        if let Some(level) = heading_level(line) {
            let content = line.trim_start_matches('#').trim();
            let spans = parse_inline(content);
            let mut styled = vec![Span::styled(
                match level {
                    1 => "# ",
                    2 => "## ",
                    3 => "### ",
                    4 => "#### ",
                    5 => "##### ",
                    _ => "###### ",
                },
                Style::default().fg(MdTheme::HEADING).add_modifier(Modifier::BOLD),
            )];
            styled.extend(spans);
            lines.push(Line::from(styled));
            continue;
        }

        if let Some(content) = line.strip_prefix("> ") {
            let mut spans = vec![Span::styled("▎ ", Style::default().fg(MdTheme::QUOTE))];
            spans.extend(parse_inline(content));
            lines.push(Line::from(spans).style(Style::default().fg(MdTheme::QUOTE)));
            continue;
        }
        if line == ">" {
            lines.push(Line::from(Span::styled("▎", Style::default().fg(MdTheme::QUOTE))));
            continue;
        }

        if let Some(content) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            let mut spans = vec![Span::styled("• ", Style::default().fg(MdTheme::LIST_BULLET))];
            spans.extend(parse_inline(content));
            lines.push(Line::from(spans));
            continue;
        }

        if let Some((num_str, content)) = strip_ordered_list_prefix(line) {
            let mut spans = vec![Span::styled(
                format!("{}. ", num_str),
                Style::default().fg(MdTheme::LIST_BULLET),
            )];
            spans.extend(parse_inline(content));
            lines.push(Line::from(spans));
            continue;
        }

        let trimmed = line.trim();
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            lines.push(Line::from(Span::styled(
                "─".repeat(50),
                Style::default().fg(MdTheme::HR).add_modifier(Modifier::DIM),
            )));
            continue;
        }

        lines.push(Line::from(parse_inline(line)));
    }

    if in_code_block {
        let block_lines = render_code_block(&code_block_content);
        lines.extend(block_lines);
    }

    lines
}

fn heading_level(line: &str) -> Option<usize> {
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
    if (1..=6).contains(&count) { Some(count) } else { None }
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

fn render_code_block(lines: &[String]) -> Vec<Line<'static>> {
    let mut result = Vec::new();
    if lines.is_empty() {
        result.push(Line::from(Span::styled(
            " ```",
            Style::default().fg(MdTheme::CODE).add_modifier(Modifier::DIM),
        )));
        return result;
    }
    for line in lines {
        result.push(Line::from(Span::styled(
            format!(" │{}", line),
            Style::default()
                .fg(MdTheme::CODE)
                .bg(MdTheme::CODE_BLOCK_BG),
        )));
    }
    result
}

fn parse_inline(text: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if chars[i] == '\\' && i + 1 < len {
            spans.push(Span::raw(chars[i + 1].to_string()));
            i += 2;
            continue;
        }

        if chars[i] == '`' {
            let start = i + 1;
            if let Some(end) = chars[start..].iter().position(|&c| c == '`') {
                let code: String = chars[start..start + end].iter().collect();
                spans.push(Span::styled(
                    code,
                    Style::default().fg(MdTheme::CODE).bg(MdTheme::CODE_BLOCK_BG),
                ));
                i = start + end + 1;
                continue;
            }
        }

        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            let start = i + 2;
            if let Some(end) = chars[start..].windows(2).position(|w| w == ['*', '*']) {
                let inner: String = chars[start..start + end].iter().collect();
                let inner_spans = parse_inline(&inner);
                let styled: Vec<Span> = inner_spans
                    .into_iter()
                    .map(|s| {
                        let style = s.style.add_modifier(Modifier::BOLD);
                        Span::styled(s.content.clone(), style)
                    })
                    .collect();
                spans.extend(styled);
                i = start + end + 2;
                continue;
            }
        }

        if chars[i] == '*' && (i + 1 >= len || chars[i + 1] != '*') {
            let start = i + 1;
            if let Some(end) = chars[start..].iter().position(|&c| c == '*') {
                let inner: String = chars[start..start + end].iter().collect();
                spans.push(Span::styled(
                    inner,
                    Style::default().fg(MdTheme::ITALIC).add_modifier(Modifier::ITALIC),
                ));
                i = start + end + 1;
                continue;
            }
        }

        if i + 1 < len && chars[i] == '~' && chars[i + 1] == '~' {
            let start = i + 2;
            if let Some(end) = chars[start..].windows(2).position(|w| w == ['~', '~']) {
                let inner: String = chars[start..start + end].iter().collect();
                spans.push(Span::styled(
                    inner,
                    Style::default()
                        .fg(MdTheme::STRIKE)
                        .add_modifier(Modifier::CROSSED_OUT),
                ));
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
                        let inner_spans = parse_inline(&text_inner);
                        let styled: Vec<Span> = inner_spans
                            .into_iter()
                            .map(|s| {
                                Span::styled(
                                    s.content.clone(),
                                    Style::default()
                                        .fg(MdTheme::LINK)
                                        .add_modifier(Modifier::UNDERLINED),
                                )
                            })
                            .collect();
                        spans.extend(styled);
                        i = url_start + close_paren + 1;
                        continue;
                    }
                }
            }
        }

        spans.push(Span::raw(chars[i].to_string()));
        i += 1;
    }

    spans
}
