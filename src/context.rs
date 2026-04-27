use crate::provider::Message;
use anyhow::Result;

/// Manages token budget and context compression
pub struct ContextManager {
    max_context: usize,
    current_tokens: usize,
    /// Rolling summary of old conversation for compaction
    summary: Option<String>,
    /// Token budget remaining for current context
    reserved_tokens: usize,
    trigger_ratio: f64,
}

impl ContextManager {
    pub fn new(max_context: usize) -> Self {
        Self {
            max_context,
            current_tokens: 0,
            summary: None,
            reserved_tokens: 50_000,
            trigger_ratio: 0.85,
        }
    }

    /// Record token usage from an LLM response
    pub fn record_usage(&mut self, prompt_tokens: usize, completion_tokens: usize) {
        // Provider usage already includes the prompt sent on this request.
        // Replace the previous estimate instead of adding to it, or a compacted
        // summary gets counted again on the next response.
        self.current_tokens = prompt_tokens.saturating_add(completion_tokens);
    }

    /// Check if context should be compacted based on token usage
    pub fn should_compact(&self, messages: &[Message]) -> bool {
        if self.summary.is_none() && messages.len() > 50 {
            return true;
        }

        let estimated_tokens = self.estimate_tokens(messages);
        estimated_tokens >= (self.max_context as f64 * self.trigger_ratio) as usize
            || estimated_tokens + self.reserved_tokens >= self.max_context
    }

    /// Compact the context — creates a summary of all but the last few messages
    pub fn compact(&mut self, messages: &[Message]) -> Result<String> {
        // In a real implementation, this would call the LLM to summarize the conversation.
        // For now, we create a simple structural summary.
        let user_msgs: Vec<_> = messages
            .iter()
            .filter_map(|m| match m {
                Message::User { content } => Some(content.clone()),
                _ => None,
            })
            .collect();

        let tool_counts = messages
            .iter()
            .filter_map(|m| match m {
                Message::Tool { content, .. } => Some(content),
                _ => None,
            })
            .count();

        let assistant_msgs = messages
            .iter()
            .filter_map(|m| match m {
                Message::Assistant { content, .. } => content.clone(),
                _ => None,
            })
            .collect::<Vec<_>>();

        let summary = format!(
            "[Previous conversation]\n\
             - User messages: {}\n\
             - Assistant responses: {}\n\
             - Tool calls made: {}\n\
             - Topics discussed: {}\n\
             Compaction triggered at ~{}% context usage.\n\
             See session history for full details.",
            user_msgs.len(),
            assistant_msgs.len(),
            tool_counts,
            user_msgs.last().unwrap_or(&"—".to_string()),
            (self.current_tokens as f64 / self.max_context as f64 * 100.0) as u64,
        );

        self.summary = Some(summary.clone());
        self.current_tokens = self.estimate_tokens_str(&summary);

        Ok(summary)
    }

    /// Rough token estimation for messages
    fn estimate_tokens(&self, messages: &[Message]) -> usize {
        let text: String = messages
            .iter()
            .map(|m| match m {
                Message::User { content } => content.clone(),
                Message::Assistant { content, .. } => content.clone().unwrap_or_default(),
                Message::Tool { content, .. } => content.clone(),
                Message::System { content } => content.clone(),
            })
            .collect::<Vec<_>>()
            .join(" ");

        self.estimate_tokens_str(&text)
    }

    fn estimate_tokens_str(&self, text: &str) -> usize {
        // Rough estimate: ~4 characters per token for English text
        text.len() / 4 + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager(max_context: usize) -> ContextManager {
        ContextManager {
            max_context,
            current_tokens: 0,
            summary: None,
            reserved_tokens: 0,
            trigger_ratio: 0.85,
        }
    }

    fn user_with_len(len: usize) -> Message {
        Message::User {
            content: "x".repeat(len),
        }
    }

    fn user(content: impl Into<String>) -> Message {
        Message::User {
            content: content.into(),
        }
    }

    #[test]
    fn compaction_triggers_at_ratio_boundary() {
        let ctx = manager(100);

        assert!(!ctx.should_compact(&[user_with_len(335)]));
        assert!(ctx.should_compact(&[user_with_len(336)]));
        assert!(ctx.should_compact(&[user_with_len(340)]));
    }

    #[test]
    fn messages_below_trigger_ratio_are_never_modified() {
        let ctx = manager(100);
        let messages = vec![user("small prompt")];
        let before = format!("{messages:?}");

        assert!(!ctx.should_compact(&messages));
        let after = format!("{messages:?}");

        assert_eq!(before, after);
        assert!(ctx.summary.is_none());
    }

    #[test]
    fn compact_summary_preserves_latest_user_input() {
        let mut ctx = manager(100);
        let messages = vec![
            user("first topic"),
            Message::Assistant {
                content: Some("assistant answer".into()),
                tool_calls: None,
                reasoning_content: None,
            },
            user("latest user request"),
        ];

        let summary = ctx.compact(&messages).unwrap();

        assert!(summary.contains("latest user request"));
    }

    #[test]
    fn record_usage_after_compaction_does_not_double_count_summary_tokens() {
        let mut ctx = manager(1000);
        let summary = ctx.compact(&[user("latest user request")]).unwrap();
        let summary_tokens = ctx.estimate_tokens_str(&summary);
        assert_eq!(ctx.current_tokens, summary_tokens);

        ctx.record_usage(10, 5);

        assert_eq!(ctx.current_tokens, 15);
    }

    #[test]
    fn long_history_heuristic_fires_only_once_per_session() {
        let mut ctx = manager(1_000_000);
        let messages = (0..51)
            .map(|n| user(format!("turn {n}")))
            .collect::<Vec<_>>();

        assert!(ctx.should_compact(&messages));
        ctx.compact(&messages).unwrap();

        assert!(!ctx.should_compact(&messages));
    }
}
