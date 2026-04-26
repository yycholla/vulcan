use std::collections::HashMap;

use ratatui::text::Line;

use super::state::{ChatMessage, ChatRole};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ChatRenderOptions {
    pub show_reasoning: bool,
    pub dense: bool,
    pub width: u16,
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
    render_count_for_tests: usize,
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
        let all = self.render_all_for_now(messages, options);
        let total_lines = all.len();
        let start = usize::from(scroll).min(total_lines);
        let end = start.saturating_add(usize::from(height)).min(total_lines);

        VisibleChatLines {
            lines: all[start..end].to_vec(),
            total_lines,
        }
    }

    fn render_all_for_now(
        &mut self,
        messages: &[ChatMessage],
        options: ChatRenderOptions,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for (index, message) in messages.iter().enumerate() {
            lines.extend(
                self.render_message_block(index, message, options)
                    .lines
                    .iter()
                    .cloned(),
            );
        }
        lines
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

        self.blocks.entry(key).or_insert_with(|| {
            self.render_count_for_tests = self.render_count_for_tests.saturating_add(1);
            RenderedMessageBlock {
                lines: vec![Line::from(message.content.clone())],
            }
        })
    }

    #[cfg(test)]
    pub fn render_count_for_tests(&self) -> usize {
        self.render_count_for_tests
    }
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
        };
        let narrow = ChatRenderOptions { width: 20, ..wide };

        let _ = store.visible_lines(&messages, wide, 0, 10, None, 0);
        let renders_after_wide = store.render_count_for_tests();
        let _ = store.visible_lines(&messages, wide, 0, 10, None, 0);
        assert_eq!(store.render_count_for_tests(), renders_after_wide);
        let _ = store.visible_lines(&messages, narrow, 0, 10, None, 0);
        assert!(store.render_count_for_tests() > renders_after_wide);
    }
}
