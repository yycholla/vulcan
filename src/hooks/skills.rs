//! YYC-243: on_context hook implementing the agentskills.io activation
//! contract. The catalog (skill name + description) is injected on every
//! turn so the model knows what's available; full SKILL.md bodies are
//! loaded lazily and injected only when the latest user message looks
//! like it matches a skill's description or name.
//!
//! Matching heuristic: tokens from a skill's `name` and `description` are
//! lower-cased and stop-words filtered, then the latest user message is
//! checked for a substring hit on at least one token of length ≥ 4. This
//! is deliberately conservative — false positives on a token like "the"
//! would defeat the activation gate. The agent itself has the final say
//! once a body is in context.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::provider::Message;
use crate::skills::{Skill, SkillRegistry};

use super::{HookHandler, HookOutcome, InjectPosition};

const MIN_TOKEN_LEN: usize = 4;

const STOP_WORDS: &[&str] = &[
    "with", "from", "this", "that", "your", "into", "when", "what", "where", "which", "while",
    "have", "been", "they", "them", "than", "then", "their", "there", "these", "those", "such",
    "user", "agent", "skill", "task", "tasks", "tool", "tools", "step", "steps", "phase", "phases",
    "use", "uses", "using",
];

pub struct SkillsHook {
    skills: Arc<SkillRegistry>,
}

impl SkillsHook {
    pub fn new(skills: Arc<SkillRegistry>) -> Self {
        Self { skills }
    }
}

/// Find skills whose name or description shares a non-trivial token with
/// the latest user message. Returns matches in catalog order.
fn matched_skills<'a>(skills: &'a [Skill], latest_user: &str) -> Vec<&'a Skill> {
    let needle = latest_user.to_lowercase();
    skills
        .iter()
        .filter(|s| {
            activation_tokens(s)
                .into_iter()
                .any(|tok| needle.contains(&tok))
        })
        .collect()
}

/// Tokens drawn from a skill's name + description that are eligible to
/// trigger activation. Lower-cased, stop-word filtered, length ≥ 4.
fn activation_tokens(skill: &Skill) -> Vec<String> {
    let mut out = Vec::new();
    for source in [skill.name.as_str(), skill.description.as_str()] {
        for raw in source.split(|c: char| !c.is_alphanumeric()) {
            let lower = raw.to_lowercase();
            if lower.len() < MIN_TOKEN_LEN {
                continue;
            }
            if STOP_WORDS.contains(&lower.as_str()) {
                continue;
            }
            if !out.contains(&lower) {
                out.push(lower);
            }
        }
    }
    out
}

/// Pull the most-recent user message text out of the running history.
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
        // Run before user-supplied hooks so other BeforePrompt handlers
        // see the skills section already in place if they care to inspect
        // it.
        10
    }

    async fn on_context(
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
             Skill bodies load lazily — when a user message matches a skill's name or \
             description the full SKILL.md body appears below this catalog. The rest \
             of the time you only see the listing. Each skill folder may also contain \
             `scripts/`, `references/`, or `assets/` subdirectories that you can read \
             on demand via your file tools using the listed `skill_root`.\n\n{listing}",
        )];

        if let Some(user_msg) = latest_user_message(messages) {
            for skill in matched_skills(all, user_msg) {
                let body = match skill.load_body() {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(
                            "load body for skill `{}` at {}: {e}",
                            skill.name,
                            skill.skill_root.display()
                        );
                        continue;
                    }
                };
                sections.push(format!(
                    "## Skill: {name}\n_{description}_\nskill_root: `{root}`\n\n{body}",
                    name = skill.name,
                    description = skill.description,
                    root = skill.skill_root.display(),
                    body = body.trim(),
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
    use std::path::PathBuf;

    fn skill(name: &str, description: &str) -> Skill {
        Skill {
            name: name.into(),
            description: description.into(),
            version: None,
            license: None,
            allowed_tools: vec![],
            skill_root: PathBuf::from("/tmp/unused"),
        }
    }

    #[test]
    fn activation_tokens_drop_short_and_stop_words() {
        let s = skill(
            "debug",
            "Systematic 4-phase debugging workflow for the user",
        );
        let toks = activation_tokens(&s);
        assert!(toks.contains(&"debug".to_string()));
        assert!(toks.contains(&"systematic".to_string()));
        assert!(toks.contains(&"debugging".to_string()));
        assert!(toks.contains(&"workflow".to_string()));
        assert!(!toks.contains(&"the".to_string())); // too short
        assert!(!toks.contains(&"user".to_string())); // stop word
    }

    #[test]
    fn matched_skills_finds_substring_hit_on_description_token() {
        let s = vec![skill(
            "debug",
            "Systematic 4-phase debugging workflow when reproducing a bug or error",
        )];
        let hits = matched_skills(&s, "I keep hitting an Error in main.rs and need help");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "debug");
    }

    #[test]
    fn matched_skills_returns_empty_on_unrelated_message() {
        let s = vec![skill("debug", "Systematic debugging workflow")];
        let hits = matched_skills(&s, "What's the weather?");
        assert!(hits.is_empty());
    }

    #[test]
    fn matched_skills_preserves_catalog_order_for_overlap() {
        let s = vec![
            skill("debug", "fix workflow"),
            skill("review", "fix review playbook"),
        ];
        let hits = matched_skills(&s, "please fix this regression — call workflow review");
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
