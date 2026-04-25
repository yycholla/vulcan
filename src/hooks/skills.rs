//! Built-in BeforePrompt hook that exposes the user's available skills to the
//! LLM. Replaces the old hard-coded skills section in PromptBuilder so skills
//! flow through the same extension surface as everything else.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::provider::Message;
use crate::skills::SkillRegistry;

use super::{HookHandler, HookOutcome, InjectPosition};

pub struct SkillsHook {
    skills: Arc<SkillRegistry>,
}

impl SkillsHook {
    pub fn new(skills: Arc<SkillRegistry>) -> Self {
        Self { skills }
    }
}

#[async_trait]
impl HookHandler for SkillsHook {
    fn name(&self) -> &str {
        "skills"
    }

    fn priority(&self) -> i32 {
        // Run before user-supplied hooks so other BeforePrompt handlers see the
        // skills section already in place if they care to inspect it.
        10
    }

    async fn before_prompt(
        &self,
        _messages: &[Message],
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        if self.skills.is_empty() {
            return Ok(HookOutcome::Continue);
        }

        let listing = self
            .skills
            .list()
            .iter()
            .map(|s| format!("- **{}**: {}", s.name, s.description))
            .collect::<Vec<_>>()
            .join("\n");

        let content = format!(
            "## Available Skills\n\
             You have learned the following skills from past experience. Load one by responding \
             with the skill name when relevant:\n\n{listing}",
        );

        Ok(HookOutcome::InjectMessages {
            messages: vec![Message::System { content }],
            position: InjectPosition::AfterSystem,
        })
    }
}
