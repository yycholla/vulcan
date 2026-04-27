//! Built-in BeforePrompt hook that exposes the user's available skills to the
//! LLM. Replaces the old hard-coded skills section in PromptBuilder so skills
//! flow through the same extension surface as everything else.
//!
//! Lazy-load behavior (YYC-37 follow-up): every turn the hook injects a
//! short catalog (name + description per skill) so the model knows what
//! exists. The full skill body is *only* injected when the latest user
//! message matches one of that skill's `triggers`. This avoids dumping
//! every skill's playbook into context on every turn while still letting
//! the model see the relevant guidance when it's actually applicable.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::provider::Message;
use crate::skills::{Skill, SkillRegistry};

use super::{HookHandler, HookOutcome, InjectPosition};

pub struct SkillsHook {
    skills: Arc<SkillRegistry>,
}

impl SkillsHook {
    pub fn new(skills: Arc<SkillRegistry>) -> Self {
        Self { skills }
    }
}

/// Find skills whose `triggers` appear (case-insensitive substring) in
/// the latest user message. The match list preserves catalog order so
/// behavior is deterministic when multiple skills overlap.
fn matched_skills<'a>(skills: &'a [Skill], latest_user: &str) -> Vec<&'a Skill> {
    let needle = latest_user.to_lowercase();
    skills
        .iter()
        .filter(|s| {
            s.triggers
                .iter()
                .any(|t| !t.is_empty() && needle.contains(&t.to_lowercase()))
        })
        .collect()
}

/// Pull the most-recent user message text out of the running history.
/// Skill triggers match on the user side — the assistant's own output
/// isn't considered, which matches how a human would discover relevant
/// playbooks (the user describes the task; the agent looks up).
fn latest_user_message(messages: &[Message]) -> Option<&str> {
    messages.iter().rev().find_map(|m| match m {
        Message::User { content } => Some(content.as_str()),
        _ => None,
    })
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
        messages: &[Message],
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        if self.skills.is_empty() {
            return Ok(HookOutcome::Continue);
        }

        let all = self.skills.list();
        let listing = all
            .iter()
            .map(|s| format!("- **{}**: {}", s.name, s.description))
            .collect::<Vec<_>>()
            .join("\n");

        let mut sections = vec![format!(
            "## Available Skills\n\
             Skill playbooks load lazily — when a user message matches one of \
             a skill's triggers the full body appears below this catalog. The \
             rest of the time you only see the listing.\n\n{listing}",
        )];

        if let Some(user_msg) = latest_user_message(messages) {
            let matched = matched_skills(all, user_msg);
            for skill in matched {
                sections.push(format!(
                    "## Skill: {}\n_{description}_\n\n{body}",
                    skill.name,
                    description = skill.description,
                    body = skill.content.trim(),
                ));
            }
        }

        Ok(HookOutcome::InjectMessages {
            messages: vec![Message::System {
                content: sections.join("\n\n"),
            }],
            position: InjectPosition::AfterSystem,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, triggers: &[&str], body: &str) -> Skill {
        Skill {
            name: name.into(),
            description: format!("desc for {name}"),
            triggers: triggers.iter().map(|t| (*t).into()).collect(),
            content: body.into(),
        }
    }

    #[test]
    fn matched_skills_finds_case_insensitive_substring() {
        let s = vec![
            skill("debug", &["bug", "error", "doesn't work"], "DEBUG_BODY"),
            skill("review", &["review this", "code review"], "REVIEW_BODY"),
        ];
        let hits = matched_skills(&s, "Help me Debug This Error in main.rs");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "debug");
    }

    #[test]
    fn matched_skills_returns_empty_when_no_trigger_hits() {
        let s = vec![skill("debug", &["bug", "error"], "BODY")];
        let hits = matched_skills(&s, "What's the weather like?");
        assert!(hits.is_empty());
    }

    #[test]
    fn matched_skills_skips_empty_triggers() {
        let s = vec![skill("oops", &["", "  "], "BODY")];
        let hits = matched_skills(&s, "anything goes");
        assert!(hits.is_empty(), "empty triggers should never match");
    }

    #[test]
    fn matched_skills_preserves_catalog_order_for_overlapping_skills() {
        let s = vec![
            skill("debug", &["fix"], "A"),
            skill("review", &["fix"], "B"),
        ];
        let hits = matched_skills(&s, "please fix this");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].name, "debug");
        assert_eq!(hits[1].name, "review");
    }

    #[test]
    fn latest_user_message_picks_most_recent_user_turn() {
        let messages = vec![
            Message::System {
                content: "sys".into(),
            },
            Message::User {
                content: "first".into(),
            },
            Message::Assistant {
                content: Some("ack".into()),
                tool_calls: None,
                reasoning_content: None,
            },
            Message::User {
                content: "latest".into(),
            },
        ];
        assert_eq!(latest_user_message(&messages), Some("latest"));
    }

    #[test]
    fn latest_user_message_returns_none_when_history_is_assistant_only() {
        let messages = vec![Message::Assistant {
            content: Some("hi".into()),
            tool_calls: None,
            reasoning_content: None,
        }];
        assert_eq!(latest_user_message(&messages), None);
    }
}
