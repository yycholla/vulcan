//! BeforePrompt hook that injects relevant Cortex graph knowledge into
//! every LLM turn (YYC-264).
//!
//! On each turn, the latest user message is used as a semantic query against
//! the Cortex knowledge graph. The top hits are injected as a System message
//! at AfterSystem position — same shape as SkillsHook and RecallHook.
//!
//! Falls through silently when cortex is disabled, empty, or the query
//! returns no relevant results.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::memory::cortex::CortexStore;
use crate::provider::Message;

use super::{HookHandler, HookOutcome, InjectPosition};

pub struct CortexRecallHook {
    store: Arc<CortexStore>,
    max_results: usize,
}

impl CortexRecallHook {
    pub fn new(store: Arc<CortexStore>, max_results: usize) -> Self {
        Self { store, max_results }
    }
}

#[async_trait]
impl HookHandler for CortexRecallHook {
    fn name(&self) -> &str {
        "cortex-recall"
    }

    /// Run after SkillsHook (10) and RecallHook (15) so skills and
    /// past-session context appear first — cortex knowledge is supplementary.
    fn priority(&self) -> i32 {
        20
    }

    async fn before_prompt(
        &self,
        messages: &[Message],
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        // Find the latest user message to use as the query.
        let query = messages.iter().rev().find_map(|m| match m {
            Message::User { content } => Some(content.clone()),
            _ => None,
        });
        let Some(query) = query else {
            return Ok(HookOutcome::Continue);
        };
        if query.trim().is_empty() {
            return Ok(HookOutcome::Continue);
        }

        // Search the graph semantically.
        let results = match self.store.search(&query, self.max_results) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("cortex-recall search failed: {e}");
                return Ok(HookOutcome::Continue);
            }
        };

        if results.is_empty() {
            return Ok(HookOutcome::Continue);
        }

        // Format as a system-level knowledge block.
        let mut body = String::from("📚 Relevant knowledge:\n");
        for (score, node) in &results {
            use std::fmt::Write;
            let pct = (score * 100.0) as u8;
            let title = &node.data.title;
            let kind = node.kind.as_str();
            let _ = writeln!(body, "• [{kind} ({pct}%)] {title}");
        }
        body.push_str("\nUse this context when forming your response.");

        let msg = Message::System { content: body };

        Ok(HookOutcome::InjectMessages {
            messages: vec![msg],
            position: InjectPosition::AfterSystem,
        })
    }
}
