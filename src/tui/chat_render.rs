use std::collections::HashMap;

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::{
    markdown::render_markdown,
    state::{ChatMessage, ChatRole, MessageSegment},
    theme::Palette,
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
    render_count: usize,
    materialized_line_count: usize,
}

impl ChatRenderStore {
    pub fn visible_lines(
        &mut self,
        messages: &[ChatMessage],
        options: ChatRenderOptions,
        scroll: u16,
        height: u16,
        _pending_pause: Option<&crate::pause::AgentPause>,
        _queue_len: usize,
    ) -> VisibleChatLines {
        self.visible_lines_at(messages, options, usize::from(scroll), usize::from(height))
    }

    pub fn visible_lines_at(
        &mut self,
        messages: &[ChatMessage],
        options: ChatRenderOptions,
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
            let block = self.render_message_block(index, message, options);
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
    }

    fn render_message_block(
        &mut self,
        index: usize,
        message: &ChatMessage,
        options: ChatRenderOptions,
    ) -> &RenderedMessageBlock {
        let key = MessageRenderKey {
            index,
            version: message.render_version(),
            role: message.role,
            options,
        };

        if !self.blocks.contains_key(&key) {
            self.render_count = self.render_count.saturating_add(1);
            let block = self.build_message_block(index, message, options);
            self.blocks.insert(key, block);
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
    ) -> RenderedMessageBlock {
        let (role_label, accent) = match message.role {
            ChatRole::User => ("you", Palette::RED),
            ChatRole::Agent => ("agent", Palette::INK),
            ChatRole::System => ("system", Palette::YELLOW),
        };
        let is_agent = matches!(message.role, ChatRole::Agent);
        let mut lines = vec![message_header(role_label, accent, None)];

        if is_agent && !message.segments.is_empty() {
            let mut text_emitted = false;
            for segment in &message.segments {
                match segment {
                    MessageSegment::Reasoning(reasoning)
                        if options.show_reasoning && !reasoning.is_empty() =>
                    {
                        lines.extend(reasoning_lines(reasoning, false));
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
                        push_markdown_body(&mut lines, text, accent);
                    }
                    MessageSegment::Text(_) => {}
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
                lines.extend(reasoning_lines(&message.reasoning, false));
            }
            if is_agent && message.content.is_empty() {
                lines.push(agent_placeholder(
                    !message.reasoning.is_empty(),
                    options.muted_style,
                ));
            } else {
                push_markdown_body(&mut lines, &message.content, accent);
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

fn push_markdown_body(lines: &mut Vec<Line<'static>>, text: &str, accent: ratatui::style::Color) {
    for line in render_markdown(text) {
        let mut spans = vec![Span::styled("▎ ", Style::default().fg(accent))];
        spans.extend(line.spans.into_iter());
        lines.push(Line::from(spans));
    }
}

fn agent_placeholder(has_reasoning: bool, muted: Style) -> Line<'static> {
    let label = if has_reasoning {
        "▎ Answering…"
    } else {
        "▎ Thinking…"
    };
    Line::from(Span::styled(
        label,
        muted.add_modifier(Modifier::SLOW_BLINK),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::{ChatMessage, ChatRole};

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

        let window = store.visible_lines(&messages, options, 10, 5, None, 0);
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

        let _ = store.visible_lines(&messages, wide, 0, 10, None, 0);
        let renders_after_wide = store.render_count_for_tests();
        let _ = store.visible_lines(&messages, wide, 0, 10, None, 0);
        assert_eq!(store.render_count_for_tests(), renders_after_wide);
        let _ = store.visible_lines(&messages, narrow, 0, 10, None, 0);
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

        let window = store.visible_lines(&messages, options, 90, 3, None, 0);

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

        let window = store.visible_lines(&messages, options, 4_900, 20, None, 0);

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
        let first = store.visible_lines_at(&messages, options, 0, 20);
        let tail_scroll = first.total_lines.saturating_sub(20);

        let window = store.visible_lines_at(&messages, options, tail_scroll, 20);
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

        let _ = store.visible_lines(&messages, options, 0, 20, None, 0);
        let first_count = store.render_count_for_tests();

        messages[1].append_text("hello");
        let _ = store.visible_lines(&messages, options, 0, 20, None, 0);

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

        let block = store.render_message_block(0, &message, options);
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
        assert!(rendered.iter().any(|line| line.starts_with("▎ ")));
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

        let block = store.render_message_block(0, &message, options);
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
}
