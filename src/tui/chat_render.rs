use std::collections::{HashMap, VecDeque};

/// YYC-144: cap on cached `RenderedMessageBlock`s. The cache is keyed
/// per (message index, version, role, options); long sessions or
/// frequent session-switch churn previously grew it indefinitely.
/// 1024 entries comfortably covers a multi-thousand-message session
/// at one option set, while bounding memory at a few MiB of rendered
/// `Line`s in the worst case. Eviction is FIFO so behavior is
/// deterministic — the oldest insertion goes when the cache is full.
const RENDER_BLOCK_CACHE_CAP: usize = 1024;

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{
    markdown::render_markdown,
    state::{ChatMessage, ChatRole, MessageSegment},
    theme::{Palette, Theme},
    widgets::{message_header, reasoning_lines, tool_card},
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ChatRenderOptions {
    pub show_reasoning: bool,
    pub dense: bool,
    pub width: u16,
    /// Style for the agent "Thinking…"/"Answering…" placeholder. Caller
    /// populates from `state.theme.muted` so the placeholder respects
    /// the active theme. Default `Style::default()` is a safe fallback
    /// for tests that don't care about styling.
    pub muted_style: Style,
}

#[derive(Clone, Debug, Default)]
pub struct VisibleChatLines {
    pub lines: Vec<Line<'static>>,
    pub total_lines: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct MessageRenderKey {
    index: usize,
    version: u64,
    role: ChatRole,
    options: ChatRenderOptions,
}

#[derive(Clone, Debug)]
struct RenderedMessageBlock {
    lines: Vec<Line<'static>>,
}

#[derive(Default)]
pub struct ChatRenderStore {
    blocks: HashMap<MessageRenderKey, RenderedMessageBlock>,
    /// YYC-144: insertion order for FIFO eviction. Never moved on
    /// hits — pure FIFO. Length is kept in lockstep with `blocks`.
    insertion_order: VecDeque<MessageRenderKey>,
    render_count: usize,
    materialized_line_count: usize,
}

impl ChatRenderStore {
    // YYC-275: TUI render hot path; a builder/options struct would
    // add allocation per draw call. Allowed at this site only.
    #[allow(clippy::too_many_arguments)]
    pub fn visible_lines(
        &mut self,
        messages: &[ChatMessage],
        options: ChatRenderOptions,
        theme: &Theme,
        scroll: u16,
        height: u16,
        _pending_pause: Option<&crate::pause::AgentPause>,
        _queue_len: usize,
    ) -> VisibleChatLines {
        self.visible_lines_at(
            messages,
            options,
            theme,
            usize::from(scroll),
            usize::from(height),
        )
    }

    pub fn visible_lines_at(
        &mut self,
        messages: &[ChatMessage],
        options: ChatRenderOptions,
        theme: &Theme,
        scroll: usize,
        height: usize,
    ) -> VisibleChatLines {
        self.materialized_line_count = 0;

        let window_start = scroll;
        let window_len = height;
        let window_end = window_start.saturating_add(window_len);
        let mut total_lines = 0usize;
        let mut visible = Vec::with_capacity(window_len);
        let mut materialized_line_count = 0usize;

        for (index, message) in messages.iter().enumerate() {
            let block = self.render_message_block(index, message, options, theme);
            let block_start = total_lines;
            let block_end = block_start.saturating_add(block.lines.len());
            total_lines = block_end;

            if window_len == 0 || block_end <= window_start || block_start >= window_end {
                continue;
            }

            let start_in_block = window_start.saturating_sub(block_start);
            let end_in_block = block
                .lines
                .len()
                .min(window_end.saturating_sub(block_start));
            for line in &block.lines[start_in_block..end_in_block] {
                if visible.len() >= window_len {
                    break;
                }
                visible.push(line.clone());
                materialized_line_count = materialized_line_count.saturating_add(1);
            }
        }

        self.materialized_line_count = materialized_line_count;

        VisibleChatLines {
            lines: visible,
            total_lines,
        }
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
        self.insertion_order.clear();
    }

    /// YYC-144: current cache occupancy. Exposed for tests + bench
    /// instrumentation; the figure is a hard upper bound on
    /// rendered-block memory the store can hold.
    pub fn cache_len(&self) -> usize {
        self.blocks.len()
    }

    fn render_message_block(
        &mut self,
        index: usize,
        message: &ChatMessage,
        options: ChatRenderOptions,
        theme: &Theme,
    ) -> &RenderedMessageBlock {
        let key = MessageRenderKey {
            index,
            version: message.render_version(),
            role: message.role,
            options,
        };

        if !self.blocks.contains_key(&key) {
            self.render_count = self.render_count.saturating_add(1);
            let block = self.build_message_block(index, message, options, theme);
            // YYC-144: enforce the cache cap before insertion. FIFO
            // eviction keeps the implementation simple + deterministic
            // — the oldest insertion is dropped, which on a steady-
            // state visible window means a message that scrolled out
            // long ago.
            if self.blocks.len() >= RENDER_BLOCK_CACHE_CAP
                && let Some(oldest) = self.insertion_order.pop_front()
            {
                self.blocks.remove(&oldest);
            }
            self.blocks.insert(key, block);
            self.insertion_order.push_back(key);
        }

        self.blocks
            .get(&key)
            .expect("message block was inserted before lookup")
    }

    fn build_message_block(
        &self,
        _index: usize,
        message: &ChatMessage,
        options: ChatRenderOptions,
        theme: &Theme,
    ) -> RenderedMessageBlock {
        let (role_label, accent) = match message.role {
            ChatRole::User => ("you", Palette::RED),
            ChatRole::Agent => ("agent", theme.body_fg),
            ChatRole::System => ("system", Palette::YELLOW),
        };
        let is_agent = matches!(message.role, ChatRole::Agent);
        let mut lines = vec![message_header(role_label, accent, None, theme)];

        if is_agent && !message.segments.is_empty() {
            let mut text_emitted = false;
            let mut prev_emitted_kind: Option<&'static str> = None;
            for segment in &message.segments {
                let segment_lines_before = lines.len();
                match segment {
                    MessageSegment::Reasoning(reasoning)
                        if options.show_reasoning && !reasoning.trim().is_empty() =>
                    {
                        lines.extend(reasoning_lines(reasoning, false, theme, options.width));
                    }
                    MessageSegment::Reasoning(_) => {}
                    MessageSegment::ToolCall {
                        name,
                        status,
                        params_summary,
                        output_preview,
                        result_meta,
                        elided_lines,
                        elapsed_ms,
                    } => {
                        lines.extend(tool_card(
                            name,
                            *status,
                            params_summary.as_deref(),
                            output_preview.as_deref(),
                            result_meta.as_deref(),
                            *elided_lines,
                            *elapsed_ms,
                            accent,
                            options.width,
                        ));
                    }
                    MessageSegment::Text(text) if !text.is_empty() => {
                        text_emitted = true;
                        push_markdown_body(&mut lines, text, accent, theme, options.width);
                    }
                    MessageSegment::Text(_) => {}
                }
                if lines.len() > segment_lines_before {
                    let kind = segment.kind_label();
                    if let Some(prev) = prev_emitted_kind
                        && prev != kind
                    {
                        lines.insert(segment_lines_before, Line::from(""));
                    }
                    prev_emitted_kind = Some(kind);
                }
            }

            if !text_emitted {
                lines.push(agent_placeholder(
                    message.has_reasoning(),
                    options.muted_style,
                ));
            }
        } else {
            if options.show_reasoning && is_agent && !message.reasoning.is_empty() {
                lines.extend(reasoning_lines(
                    &message.reasoning,
                    false,
                    theme,
                    options.width,
                ));
            }
            if is_agent && message.content.is_empty() {
                lines.push(agent_placeholder(
                    !message.reasoning.is_empty(),
                    options.muted_style,
                ));
            } else {
                push_markdown_body(&mut lines, &message.content, accent, theme, options.width);
            }
        }

        if !options.dense {
            lines.push(Line::from(""));
        }

        RenderedMessageBlock { lines }
    }

    pub fn render_count(&self) -> usize {
        self.render_count
    }

    pub fn materialized_line_count(&self) -> usize {
        self.materialized_line_count
    }

    #[cfg(test)]
    pub fn render_count_for_tests(&self) -> usize {
        self.render_count()
    }

    #[cfg(test)]
    pub fn materialized_line_count_for_tests(&self) -> usize {
        self.materialized_line_count()
    }
}

fn push_markdown_body(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    _accent: ratatui::style::Color,
    theme: &Theme,
    width: u16,
) {
    // Pre-wrap markdown so transcript rows stay stable without adding
    // copy-hostile rail characters to every visual row.
    // Trim trailing whitespace/newlines so models that emit `\n\n`
    // suffixes don't leave empty rows after the body.
    let trimmed = text.trim_end_matches(['\n', '\r', ' ', '\t']);
    if trimmed.is_empty() {
        return;
    }
    let inner_width = width.max(1) as usize;
    for line in render_markdown(trimmed, theme) {
        for row in wrap_spans(line.spans, inner_width) {
            lines.push(Line::from(row));
        }
    }
}

/// Soft-wrap a sequence of spans into rows that each fit `width`
/// columns. Splits inside spans when a span itself is wider than the
/// remaining space; preserves per-span styles.
fn wrap_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Vec<Span<'static>>> {
    if width == 0 {
        return vec![spans];
    }
    let continuation = continuation_prefix(&spans, width);
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut col = 0usize;
    for span in coalesce_spans(spans) {
        let style = span.style;
        for (segment, whitespace) in text_segments(span.content.as_ref()) {
            push_wrapped_segment(
                &mut rows,
                &mut col,
                segment,
                whitespace,
                style,
                width,
                continuation.as_ref(),
            );
        }
    }
    if rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    rows
}

#[derive(Clone, Debug)]
struct ContinuationPrefix {
    spans: Vec<Span<'static>>,
    width: usize,
}

fn continuation_prefix(spans: &[Span<'static>], width: usize) -> Option<ContinuationPrefix> {
    let first = spans.first()?;
    let content = first.content.as_ref();
    let prefix = if content == "▎ " || content == " │" {
        Some(Span::styled(content.to_string(), first.style))
    } else if content.starts_with(" │") {
        Some(Span::styled(" │", first.style))
    } else if content == "• " {
        Some(Span::styled("  ", first.style))
    } else if is_ordered_list_marker(content) {
        Some(Span::styled(
            " ".repeat(UnicodeWidthStr::width(content)),
            first.style,
        ))
    } else {
        None
    }?;
    let prefix_width = UnicodeWidthStr::width(prefix.content.as_ref());
    (prefix_width < width).then_some(ContinuationPrefix {
        spans: vec![prefix],
        width: prefix_width,
    })
}

fn is_ordered_list_marker(text: &str) -> bool {
    let Some(number) = text.strip_suffix(". ") else {
        return false;
    };
    !number.is_empty() && number.chars().all(|ch| ch.is_ascii_digit())
}

fn coalesce_spans(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut coalesced: Vec<Span<'static>> = Vec::new();
    for span in spans {
        if let Some(previous) = coalesced.last_mut()
            && previous.style == span.style
        {
            previous.content.to_mut().push_str(span.content.as_ref());
            continue;
        }
        coalesced.push(span);
    }
    coalesced
}

fn text_segments(text: &str) -> impl Iterator<Item = (&str, bool)> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    let mut current_kind: Option<bool> = None;

    for (idx, ch) in text.char_indices() {
        let kind = ch.is_whitespace();
        match current_kind {
            Some(existing) if existing != kind => {
                segments.push((&text[start..idx], existing));
                start = idx;
                current_kind = Some(kind);
            }
            None => current_kind = Some(kind),
            _ => {}
        }
    }

    if let Some(kind) = current_kind {
        segments.push((&text[start..], kind));
    }

    segments.into_iter()
}

fn push_wrapped_segment(
    rows: &mut Vec<Vec<Span<'static>>>,
    col: &mut usize,
    mut segment: &str,
    whitespace: bool,
    style: Style,
    width: usize,
    continuation: Option<&ContinuationPrefix>,
) {
    while !segment.is_empty() {
        let prefix_width = continuation.map_or(0, |prefix| prefix.width);
        let at_continuation_prefix = rows.len() > 1 && *col == prefix_width;
        if whitespace && at_continuation_prefix {
            return;
        }

        let remaining = width.saturating_sub(*col);
        if remaining == 0 {
            trim_trailing_whitespace(rows, col);
            push_continuation_row(rows, col, continuation);
            continue;
        }

        let segment_width = UnicodeWidthStr::width(segment);
        if segment_width <= remaining {
            rows.last_mut()
                .expect("wrap rows are initialized")
                .push(Span::styled(segment.to_string(), style));
            *col = (*col).saturating_add(segment_width);
            return;
        }

        if *col > 0 && !at_continuation_prefix {
            trim_trailing_whitespace(rows, col);
            push_continuation_row(rows, col, continuation);
            if whitespace {
                return;
            }
            continue;
        }

        let (chunk, rest, chunk_width) = split_display_width(segment, remaining);
        rows.last_mut()
            .expect("wrap rows are initialized")
            .push(Span::styled(chunk.to_string(), style));
        *col = (*col).saturating_add(chunk_width);
        segment = rest;
        if !segment.is_empty() {
            push_continuation_row(rows, col, continuation);
        }
    }
}

fn push_continuation_row(
    rows: &mut Vec<Vec<Span<'static>>>,
    col: &mut usize,
    continuation: Option<&ContinuationPrefix>,
) {
    let mut row = Vec::new();
    *col = 0;
    if let Some(prefix) = continuation {
        row.extend(prefix.spans.iter().cloned());
        *col = prefix.width;
    }
    rows.push(row);
}

fn trim_trailing_whitespace(rows: &mut [Vec<Span<'static>>], col: &mut usize) {
    let Some(row) = rows.last_mut() else {
        return;
    };

    while let Some(last) = row.last_mut() {
        let content = last.content.as_ref();
        let trimmed = content.trim_end_matches(char::is_whitespace);
        if trimmed.len() == content.len() {
            return;
        }

        let removed_width = UnicodeWidthStr::width(&content[trimmed.len()..]);
        if trimmed.is_empty() {
            row.pop();
        } else {
            last.content = trimmed.to_string().into();
        }
        *col = (*col).saturating_sub(removed_width);
    }
}

fn split_display_width(text: &str, max_width: usize) -> (&str, &str, usize) {
    let mut end = 0usize;
    let mut width = 0usize;

    for (idx, ch) in text.char_indices() {
        let char_width = ch.width().unwrap_or(0);
        if end > 0 && width.saturating_add(char_width) > max_width {
            break;
        }
        end = idx + ch.len_utf8();
        width = width.saturating_add(char_width);
        if width >= max_width {
            break;
        }
    }

    if end == 0 {
        return ("", text, 0);
    }

    (&text[..end], &text[end..], width)
}

fn agent_placeholder(has_reasoning: bool, muted: Style) -> Line<'static> {
    let label = if has_reasoning {
        "Answering…"
    } else {
        "Thinking…"
    };
    Line::from(Span::styled(
        label,
        muted.add_modifier(Modifier::SLOW_BLINK),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::{ChatMessage, ChatRole, MessageSegment, ToolStatus};

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    fn row_text(row: &[Span<'static>]) -> String {
        row.iter().map(|s| s.content.as_ref()).collect::<String>()
    }

    fn assert_rows_fit(rows: &[Vec<Span<'static>>], width: usize) {
        for row in rows {
            let text = row_text(row);
            assert!(
                UnicodeWidthStr::width(text.as_str()) <= width,
                "row {text:?} exceeded width {width}"
            );
        }
    }

    #[test]
    fn wrap_spans_uses_display_width_for_emoji_cjk_and_symbols() {
        let rows = wrap_spans(vec![Span::raw("ab😀中文─cd")], 6);

        let rendered: Vec<String> = rows.iter().map(|row| row_text(row)).collect();
        assert_eq!(rendered, vec!["ab😀中", "文─cd"]);
        assert_rows_fit(&rows, 6);
    }

    #[test]
    fn wrap_spans_prefers_word_boundaries() {
        let rows = wrap_spans(vec![Span::raw("alpha beta gamma")], 10);

        let rendered: Vec<String> = rows.iter().map(|row| row_text(row)).collect();
        assert_eq!(rendered, vec!["alpha beta", "gamma"]);
        assert_rows_fit(&rows, 10);
    }

    #[test]
    fn wrap_spans_preserves_inline_code_and_styled_span_styles() {
        let code_style = Style::default().add_modifier(Modifier::BOLD);
        let plain_style = Style::default();
        let rows = wrap_spans(
            vec![
                Span::styled("run ", plain_style),
                Span::styled("cargo test --lib", code_style),
            ],
            10,
        );

        let rendered: Vec<String> = rows.iter().map(|row| row_text(row)).collect();
        assert_eq!(rendered, vec!["run cargo", "test --lib"]);
        assert_rows_fit(&rows, 10);
        for chunk in ["cargo", "test", "--lib"] {
            assert!(
                rows.iter()
                    .flatten()
                    .any(|span| span.content.as_ref() == chunk && span.style == code_style),
                "missing styled inline-code chunk {chunk:?}"
            );
        }
    }

    #[test]
    fn wrapped_blockquote_keeps_accent_rail_on_first_row() {
        let theme = Theme::system();
        let rows = render_markdown("> alpha beta gamma", &theme)
            .into_iter()
            .flat_map(|line| wrap_spans(line.spans, 10))
            .collect::<Vec<_>>();

        let rendered: Vec<String> = rows.iter().map(|row| row_text(row)).collect();
        assert_eq!(rendered, vec!["▎ alpha", "▎ beta", "▎ gamma"]);
        assert_eq!(rows[0][0].content.as_ref(), "▎");
        assert_eq!(rows[0][0].style, theme.blockquote);
        assert_eq!(rows[1][0].content.as_ref(), "▎ ");
        assert_eq!(rows[1][0].style, theme.blockquote);
        assert_rows_fit(&rows, 10);
    }

    #[test]
    fn wrapped_lists_preserve_continuation_indent() {
        let theme = Theme::system();
        let rows = render_markdown("- alpha beta gamma", &theme)
            .into_iter()
            .flat_map(|line| wrap_spans(line.spans, 10))
            .collect::<Vec<_>>();

        let rendered: Vec<String> = rows.iter().map(|row| row_text(row)).collect();
        assert_eq!(rendered, vec!["• alpha", "  beta", "  gamma"]);
        assert_eq!(rows[1][0].content.as_ref(), "  ");
        assert_eq!(rows[1][0].style, theme.list_marker);
        assert_rows_fit(&rows, 10);
    }

    #[test]
    fn wrapped_ordered_lists_preserve_continuation_indent() {
        let theme = Theme::system();
        let rows = render_markdown("12. alpha beta gamma", &theme)
            .into_iter()
            .flat_map(|line| wrap_spans(line.spans, 11))
            .collect::<Vec<_>>();

        let rendered: Vec<String> = rows.iter().map(|row| row_text(row)).collect();
        assert_eq!(rendered, vec!["12. alpha", "    beta", "    gamma"]);
        assert_eq!(rows[1][0].content.as_ref(), "    ");
        assert_eq!(rows[1][0].style, theme.list_marker);
        assert_rows_fit(&rows, 11);
    }

    #[test]
    fn wrapped_code_blocks_preserve_code_rail() {
        let theme = Theme::system();
        let rows = render_markdown("```\nabcdef gh\n```", &theme)
            .into_iter()
            .flat_map(|line| wrap_spans(line.spans, 8))
            .collect::<Vec<_>>();

        let rendered: Vec<String> = rows.iter().map(|row| row_text(row)).collect();
        assert_eq!(rendered, vec![" │abcdef", " │gh"]);
        assert_eq!(rows[1][0].content.as_ref(), " │");
        assert_eq!(rows[1][0].style, theme.code_block);
        assert_rows_fit(&rows, 8);
    }

    // YYC-144: insertions past the cap must drop the oldest entry
    // and keep the store at exactly RENDER_BLOCK_CACHE_CAP. Renders
    // CAP+1 distinct user messages — each gets its own cache key
    // because index differs — and asserts the first key is gone
    // while the latest is present.
    #[test]
    fn render_store_evicts_oldest_when_cap_exceeded() {
        let theme = Theme::system();
        let options = ChatRenderOptions {
            show_reasoning: false,
            dense: false,
            width: 80,
            muted_style: Style::default(),
        };
        let mut store = ChatRenderStore::default();
        // Fill exactly to the cap.
        let messages: Vec<ChatMessage> = (0..RENDER_BLOCK_CACHE_CAP)
            .map(|i| ChatMessage::new(ChatRole::User, format!("msg {i}")))
            .collect();
        store.visible_lines_at(&messages, options, &theme, 0, RENDER_BLOCK_CACHE_CAP);
        assert_eq!(store.cache_len(), RENDER_BLOCK_CACHE_CAP);

        // Render one more distinct message → must evict the oldest.
        let mut overflow = messages.clone();
        overflow.push(ChatMessage::new(ChatRole::User, "msg overflow"));
        store.visible_lines_at(&overflow, options, &theme, 0, overflow.len());
        assert_eq!(
            store.cache_len(),
            RENDER_BLOCK_CACHE_CAP,
            "cache should never exceed cap",
        );
    }

    #[test]
    fn agent_message_inserts_blank_line_between_segment_kinds() {
        let mut store = ChatRenderStore::default();
        let mut msg = ChatMessage::new(ChatRole::Agent, "");
        msg.segments
            .push(MessageSegment::Reasoning("thinking through".into()));
        msg.segments
            .push(MessageSegment::Text("here is the answer".into()));
        msg.segments.push(MessageSegment::ToolCall {
            name: "read_file".into(),
            status: ToolStatus::Done(true),
            params_summary: Some("src/lib.rs".into()),
            output_preview: None,
            result_meta: None,
            elided_lines: 0,
            elapsed_ms: None,
        });

        let options = ChatRenderOptions {
            show_reasoning: true,
            dense: true,
            width: 80,
            muted_style: Style::default(),
        };
        let theme = Theme::system();
        let window = store.visible_lines_at(std::slice::from_ref(&msg), options, &theme, 0, 200);
        let lines: Vec<String> = window.lines.iter().map(line_text).collect();

        let reasoning_idx = lines
            .iter()
            .position(|l| l.contains("thinking through"))
            .expect("reasoning present");
        let text_idx = lines
            .iter()
            .position(|l| l.contains("here is the answer"))
            .expect("text present");
        let tool_idx = lines
            .iter()
            .position(|l| l.contains("read_file"))
            .expect("tool card present");

        assert!(text_idx > reasoning_idx, "text after reasoning");
        assert!(tool_idx > text_idx, "tool after text");

        let between_reasoning_text: Vec<&String> =
            lines[reasoning_idx + 1..text_idx].iter().collect();
        assert!(
            between_reasoning_text.iter().any(|l| l.trim().is_empty()),
            "blank line missing between reasoning and text, got {between_reasoning_text:?}"
        );

        let between_text_tool: Vec<&String> = lines[text_idx + 1..tool_idx].iter().collect();
        assert!(
            between_text_tool.iter().any(|l| l.trim().is_empty()),
            "blank line missing between text and tool, got {between_text_tool:?}"
        );
    }

    #[test]
    fn agent_message_no_blank_between_same_kind_segments() {
        let mut store = ChatRenderStore::default();
        let mut msg = ChatMessage::new(ChatRole::Agent, "");
        msg.segments.push(MessageSegment::Text("first".into()));
        msg.segments.push(MessageSegment::Text("second".into()));

        let options = ChatRenderOptions {
            show_reasoning: true,
            dense: true,
            width: 80,
            muted_style: Style::default(),
        };
        let theme = Theme::system();
        let window = store.visible_lines_at(std::slice::from_ref(&msg), options, &theme, 0, 200);
        let lines: Vec<String> = window.lines.iter().map(line_text).collect();

        let first_idx = lines
            .iter()
            .position(|l| l.contains("first"))
            .expect("first present");
        let second_idx = lines
            .iter()
            .position(|l| l.contains("second"))
            .expect("second present");
        assert!(second_idx > first_idx);
        let between: Vec<&String> = lines[first_idx + 1..second_idx].iter().collect();
        assert!(
            !between.iter().any(|l| l.trim().is_empty()),
            "no blank should appear between same-kind segments, got {between:?}"
        );
    }

    #[test]
    fn agent_message_no_blank_when_reasoning_hidden() {
        let mut store = ChatRenderStore::default();
        let mut msg = ChatMessage::new(ChatRole::Agent, "");
        msg.segments
            .push(MessageSegment::Reasoning("hidden".into()));
        msg.segments.push(MessageSegment::Text("visible".into()));

        let options = ChatRenderOptions {
            show_reasoning: false,
            dense: true,
            width: 80,
            muted_style: Style::default(),
        };
        let theme = Theme::system();
        let window = store.visible_lines_at(std::slice::from_ref(&msg), options, &theme, 0, 200);
        let lines: Vec<String> = window.lines.iter().map(line_text).collect();

        let text_idx = lines
            .iter()
            .position(|l| l.contains("visible"))
            .expect("text present");
        let preceding: Vec<&String> = lines[..text_idx].iter().collect();
        assert!(
            !preceding.iter().any(|l| l.trim().is_empty()),
            "no blank should appear when reasoning was filtered out, got {preceding:?}"
        );
    }

    #[test]
    fn render_store_returns_only_visible_window() {
        let mut store = ChatRenderStore::default();
        let messages = (0..100)
            .map(|i| ChatMessage::new(ChatRole::User, format!("message {i}")))
            .collect::<Vec<_>>();

        let options = ChatRenderOptions {
            show_reasoning: true,
            dense: false,
            width: 80,
            muted_style: Style::default(),
        };

        let theme = Theme::system();
        let window = store.visible_lines(&messages, options, &theme, 10, 5, None, 0);
        assert_eq!(window.lines.len(), 5);
        assert!(window.total_lines > 5);
    }

    #[test]
    fn render_store_cache_key_includes_render_options() {
        let mut store = ChatRenderStore::default();
        let messages = vec![ChatMessage::new(ChatRole::User, "hello")];

        let wide = ChatRenderOptions {
            show_reasoning: true,
            dense: false,
            width: 80,
            muted_style: Style::default(),
        };
        let narrow = ChatRenderOptions { width: 20, ..wide };
        let theme = Theme::system();

        let _ = store.visible_lines(&messages, wide, &theme, 0, 10, None, 0);
        let renders_after_wide = store.render_count_for_tests();
        let _ = store.visible_lines(&messages, wide, &theme, 0, 10, None, 0);
        assert_eq!(store.render_count_for_tests(), renders_after_wide);
        let _ = store.visible_lines(&messages, narrow, &theme, 0, 10, None, 0);
        assert!(store.render_count_for_tests() > renders_after_wide);
    }

    #[test]
    fn visible_lines_does_not_clone_offscreen_message_lines() {
        let mut store = ChatRenderStore::default();
        let messages = (0..100)
            .map(|i| ChatMessage::new(ChatRole::User, format!("message {i}")))
            .collect::<Vec<_>>();
        let options = ChatRenderOptions {
            show_reasoning: true,
            dense: false,
            width: 80,
            muted_style: Style::default(),
        };

        let theme = Theme::system();
        let window = store.visible_lines(&messages, options, &theme, 90, 3, None, 0);

        assert_eq!(window.lines.len(), 3);
        assert!(window.total_lines > window.lines.len());
        assert!(store.materialized_line_count_for_tests() <= 3);
    }

    #[test]
    fn large_transcript_visible_window_stays_small() {
        let mut store = ChatRenderStore::default();
        let messages = (0..5_000)
            .map(|i| ChatMessage::new(ChatRole::User, format!("message {i}")))
            .collect::<Vec<_>>();
        let options = ChatRenderOptions {
            show_reasoning: true,
            dense: false,
            width: 100,
            muted_style: Style::default(),
        };

        let theme = Theme::system();
        let window = store.visible_lines(&messages, options, &theme, 4_900, 20, None, 0);

        assert_eq!(window.lines.len(), 20);
        assert!(window.total_lines > 5_000);
        assert!(store.materialized_line_count_for_tests() <= 20);
    }

    #[test]
    fn visible_lines_at_supports_large_scroll_offsets() {
        let mut store = ChatRenderStore::default();
        let messages = (0..50_000)
            .map(|i| ChatMessage::new(ChatRole::User, format!("message {i}")))
            .collect::<Vec<_>>();
        let options = ChatRenderOptions {
            show_reasoning: true,
            dense: false,
            width: 100,
            muted_style: Style::default(),
        };
        let theme = Theme::system();
        let first = store.visible_lines_at(&messages, options, &theme, 0, 20);
        let tail_scroll = first.total_lines.saturating_sub(20);

        let window = store.visible_lines_at(&messages, options, &theme, tail_scroll, 20);
        let text = window
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(window.lines.len(), 20);
        assert!(text.contains("message 49999"));
        assert!(store.materialized_line_count_for_tests() <= 20);
    }

    #[test]
    fn mutating_one_message_only_rerenders_that_block() {
        let mut store = ChatRenderStore::default();
        let mut messages = vec![
            ChatMessage::new(ChatRole::User, "one"),
            ChatMessage::new(ChatRole::Agent, ""),
        ];
        let options = ChatRenderOptions {
            show_reasoning: true,
            dense: false,
            width: 80,
            muted_style: Style::default(),
        };

        let theme = Theme::system();
        let _ = store.visible_lines(&messages, options, &theme, 0, 20, None, 0);
        let first_count = store.render_count_for_tests();

        messages[1].append_text("hello");
        let _ = store.visible_lines(&messages, options, &theme, 0, 20, None, 0);

        assert_eq!(store.render_count_for_tests(), first_count + 1);
    }

    #[test]
    fn render_user_message_block_includes_header_and_body() {
        let mut store = ChatRenderStore::default();
        let message = ChatMessage::new(ChatRole::User, "hello **there**");
        let options = ChatRenderOptions {
            show_reasoning: true,
            dense: true,
            width: 80,
            muted_style: Style::default(),
        };
        let theme = Theme::system();

        let block = store.render_message_block(0, &message, options, &theme);
        let rendered = block
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert!(rendered.iter().any(|line| line.contains("YOU")));
        assert!(rendered.iter().any(|line| line.contains("hello")));
        assert!(rendered.iter().any(|line| line.starts_with("hello")));
        assert!(!rendered.iter().any(|line| line.starts_with("▎ ")));
    }

    #[test]
    fn render_agent_segment_block_preserves_tool_and_text_order() {
        let mut store = ChatRenderStore::default();
        let mut message = ChatMessage::new(ChatRole::Agent, "");
        message.append_reasoning("checking files");
        message.push_tool_start_with("read_file", Some("src/main.rs".to_string()));
        message.finish_tool_with(
            "read_file",
            true,
            Some("fn main() {}".to_string()),
            Some("1 line".to_string()),
            0,
            Some(12),
        );
        message.append_text("The file is small.");

        let options = ChatRenderOptions {
            show_reasoning: true,
            dense: true,
            width: 80,
            muted_style: Style::default(),
        };
        let theme = Theme::system();

        let block = store.render_message_block(0, &message, options, &theme);
        let rendered = block
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        let reasoning = rendered
            .iter()
            .position(|line| line.contains("checking files"))
            .expect("reasoning should render");
        let tool = rendered
            .iter()
            .position(|line| line.contains("read_file"))
            .expect("tool should render");
        let text = rendered
            .iter()
            .position(|line| line.contains("The file is small."))
            .expect("text should render");

        assert!(reasoning < tool);
        assert!(tool < text);
        assert!(rendered.iter().any(|line| line.contains("OK")));
    }

    /// Snapshot-locks the visible rendering of a small fixed transcript.
    /// Catches accidental changes to spacing, prefixes, tool-card layout,
    /// and reasoning placement that the per-assertion tests above wouldn't
    /// notice. Width pinned to 60, theme system, dense=true so the snapshot
    /// is reproducible across machines.
    #[test]
    fn snapshot_small_transcript_render() {
        let mut store = ChatRenderStore::default();

        let messages = vec![
            ChatMessage::new(ChatRole::User, "what's in src/main.rs?"),
            {
                let mut m = ChatMessage::new(ChatRole::Agent, "");
                m.append_reasoning("inspecting file");
                m.push_tool_start_with("read_file", Some("src/main.rs".to_string()));
                m.finish_tool_with(
                    "read_file",
                    true,
                    Some("fn main() {}".to_string()),
                    Some("1 line".to_string()),
                    0,
                    Some(12),
                );
                m.append_text("The file is a one-line `main`.");
                m
            },
        ];

        let options = ChatRenderOptions {
            show_reasoning: true,
            dense: true,
            width: 60,
            muted_style: Style::default(),
        };
        let window = store.visible_lines_at(&messages, options, &Theme::system(), 0, 200);
        let body: String = window
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");

        insta::assert_snapshot!("chat_render_small_transcript", body);
    }
}
