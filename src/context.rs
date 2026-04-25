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
        // Simple moving estimate — total is approximate
        self.current_tokens = self
            .current_tokens
            .saturating_add(completion_tokens)
            .max(prompt_tokens);
    }

    /// Check if context should be compacted based on token usage
    pub fn should_compact(&self, messages: &[Message]) -> bool {
        if !self.summary.is_some() && messages.len() > 50 {
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
