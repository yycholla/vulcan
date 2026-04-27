//! Auto-skill creation (YYC-20).
//!
//! After a turn that took 5+ iterations, optionally ask the active
//! provider to summarize what was learned as a reusable skill
//! (name + description + triggers + content). Writes the draft to
//! `<skills_dir>/_pending/<name>.md` for manual review — never
//! installs to the active skills directory directly so the user
//! always sees a draft before it loads.
//!
//! Gated by `config.auto_create_skills`. Off by default — opting in
//! burns an extra LLM round-trip at the end of long turns.

use std::path::PathBuf;

use anyhow::Result;
use serde::Deserialize;

use crate::provider::Message;

use super::Agent;

const GENERATION_SYSTEM_PROMPT: &str = "\
You are a skill summarizer. The user just completed a multi-step task with an AI agent. \
Distill what was learned into a reusable skill the agent can load on future tasks.

Output JSON ONLY (no commentary, no code fences) matching this schema:
{
  \"name\": \"kebab-case-name\",
  \"description\": \"one line summary\",
  \"triggers\": [\"keyword\"],
  \"content\": \"markdown body with the actual instructions\"
}

Constraints:
- name: 3-40 chars, lowercase a-z 0-9 and dashes only.
- description: 10-200 chars.
- triggers: 1-5 short keywords or phrases.
- content: markdown bullets / headings; concrete, actionable, under 1000 words.";

#[derive(Debug, Deserialize)]
struct DraftSkill {
    /// Raw name from the LLM. Sanitized via `sanitize_skill_name`
    /// before it's used as a filename / frontmatter slug, so the raw
    /// field is intentionally not read directly elsewhere.
    #[allow(dead_code)]
    name: String,
    description: String,
    #[serde(default)]
    triggers: Vec<String>,
    content: String,
}

impl Agent {
    /// Generate a draft skill from the just-completed turn and write
    /// it to the pending directory. Returns the path on success.
    /// Never errors on LLM / parse failure — logs and returns
    /// `Ok(None)` so the agent loop is unaffected (YYC-20).
    pub(in crate::agent) async fn auto_create_skill_from_turn(
        &self,
        input: &str,
        response: &str,
    ) -> Result<Option<PathBuf>> {
        if !self.auto_create_skills {
            return Ok(None);
        }

        let user_prompt = format!(
            "User input:\n{}\n\nFinal agent response:\n{}\n\nGenerate the skill JSON now.",
            input, response,
        );
        let messages = vec![
            Message::System {
                content: GENERATION_SYSTEM_PROMPT.to_string(),
            },
            Message::User {
                content: user_prompt,
            },
        ];

        let cancel = self.turn_cancel.clone();
        let resp = match self.provider.chat(&messages, &[], cancel).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("auto-skill: provider call failed: {e}");
                return Ok(None);
            }
        };

        let body = resp.content.unwrap_or_default();
        let body = strip_json_fence(body.trim());
        let draft: DraftSkill = match serde_json::from_str(body) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(
                    "auto-skill: JSON parse failed ({e}). Body head: {:.100}",
                    body
                );
                return Ok(None);
            }
        };

        let safe_name = sanitize_skill_name(&draft.name);
        if safe_name.is_empty() {
            tracing::warn!("auto-skill: name sanitized to empty, skipping");
            return Ok(None);
        }

        let pending_dir = self.skills.skills_dir().join("_pending");
        std::fs::create_dir_all(&pending_dir)?;
        let path = pending_dir.join(format!("{safe_name}.md"));
        if path.exists() {
            tracing::info!("auto-skill: draft for '{safe_name}' already pending review, skipping",);
            return Ok(None);
        }

        let body = render_skill_markdown(&draft, &safe_name);
        std::fs::write(&path, body)?;
        tracing::info!("auto-skill: wrote draft to {}", path.display());
        Ok(Some(path))
    }
}

fn strip_json_fence(content: &str) -> &str {
    if let Some(rest) = content.strip_prefix("```json") {
        return rest.strip_suffix("```").unwrap_or(rest).trim();
    }
    if let Some(rest) = content.strip_prefix("```") {
        return rest.strip_suffix("```").unwrap_or(rest).trim();
    }
    content
}

/// Lowercase + restrict to `[a-z0-9-]`, drop runs of dashes, cap length.
/// Empty if nothing usable remains.
fn sanitize_skill_name(raw: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for c in raw.trim().chars() {
        let lower = c.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            out.push(lower);
            last_dash = false;
        } else if (c == '-' || c == '_' || c.is_whitespace()) && !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }

    let mut result: String = out.chars().take(40).collect();
    while result.ends_with('-') {
        result.pop();
    }
    result

}

fn render_skill_markdown(draft: &DraftSkill, sanitized_name: &str) -> String {
    let triggers: Vec<String> = draft
        .triggers
        .iter()
        .filter(|t| !t.trim().is_empty())
        .take(8)
        .map(|t| format!("\"{}\"", t.replace('"', "\\\"")))
        .collect();
    let triggers_line = if triggers.is_empty() {
        "[]".to_string()
    } else {
        format!("[{}]", triggers.join(", "))
    };
    format!(
        "---\nname: {sanitized_name}\ndescription: {description}\ntriggers: {triggers}\nauto_generated: true\n---\n\n{content}\n",
        description = draft.description.replace('\n', " "),
        triggers = triggers_line,
        content = draft.content,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_skill_name_strips_punctuation_and_lowercases() {
        assert_eq!(
            sanitize_skill_name("Fix Bash Force Push"),
            "fix-bash-force-push"
        );
        assert_eq!(sanitize_skill_name("config_migration"), "config-migration");
        assert_eq!(sanitize_skill_name("...UPPER!CASE-name?"), "uppercase-name",);
    }

    #[test]
    fn sanitize_skill_name_caps_length() {
        let raw = "a".repeat(80);
        let out = sanitize_skill_name(&raw);
        assert!(out.len() <= 40);
    }

    #[test]
    fn sanitize_skill_name_returns_empty_on_garbage() {
        assert_eq!(sanitize_skill_name("?? ! / .,"), "");
    }

    #[test]
    fn strip_json_fence_handles_json_marker_and_bare_fence() {
        assert_eq!(strip_json_fence("```json\n{\"x\":1}\n```"), "{\"x\":1}",);
        assert_eq!(strip_json_fence("```\n{\"x\":1}\n```"), "{\"x\":1}");
        assert_eq!(strip_json_fence("{\"x\":1}"), "{\"x\":1}");
    }

    #[test]
    fn render_skill_markdown_emits_frontmatter_and_body() {
        let draft = DraftSkill {
            name: "ignored-here".into(),
            description: "Fix force-push safety".into(),
            triggers: vec!["force push".into(), "git push -f".into()],
            content: "## How\n- Use --force-with-lease".into(),
        };
        let out = render_skill_markdown(&draft, "fix-force-push");
        assert!(out.starts_with("---"));
        assert!(out.contains("name: fix-force-push"));
        assert!(out.contains("description: Fix force-push safety"));
        assert!(out.contains("\"force push\""));
        assert!(out.contains("\"git push -f\""));
        assert!(out.contains("auto_generated: true"));
        assert!(out.contains("Use --force-with-lease"));
    }
}
