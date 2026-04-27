use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A loaded skill — reusable knowledge for specific task types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub triggers: Vec<String>,
    pub content: String,
}

/// Manages loading, listing, and auto-creating skills
pub struct SkillRegistry {
    skills_dir: PathBuf,
    skills: Vec<Skill>,
}

impl SkillRegistry {
    pub fn new(skills_dir: &PathBuf) -> Self {
        let dir = if skills_dir.exists() {
            skills_dir.clone()
        } else {
            // Fall back to bundled skills
            let bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills");
            if bundled.exists() {
                bundled
            } else {
                skills_dir.clone()
            }
        };

        let mut registry = Self {
            skills_dir: dir,
            skills: Vec::new(),
        };
        registry.load_all().ok();
        registry
    }

    /// Load all skill markdown files from the skills directory
    fn load_all(&mut self) -> Result<()> {
        if !self.skills_dir.exists() {
            return Ok(());
        }

        let mut skills = Vec::new();

        for entry in std::fs::read_dir(&self.skills_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md")
                && let Some(skill) = Self::load_skill(&path)? {
                    skills.push(skill);
                }
        }

        self.skills = skills;
        Ok(())
    }

    /// Parse a single skill markdown file
    fn load_skill(path: &PathBuf) -> Result<Option<Skill>> {
        let content = std::fs::read_to_string(path)?;

        // Parse YAML frontmatter
        if let Some(frontmatter) = Self::parse_frontmatter(&content) {
            let name = frontmatter
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let description = frontmatter
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let triggers: Vec<String> = frontmatter
                .get("triggers")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            // Content is everything after the frontmatter
            let body = Self::strip_frontmatter(&content);

            if !name.is_empty() {
                return Ok(Some(Skill {
                    name,
                    description,
                    triggers,
                    content: body,
                }));
            }
        }

        Ok(None)
    }

    /// Parse YAML frontmatter from markdown (basic — no full YAML parser dependency)
    fn parse_frontmatter(content: &str) -> Option<serde_json::Value> {
        let content = content.trim();
        if !content.starts_with("---") {
            return None;
        }

        let end = content[3..].find("\n---")?;
        let yaml_str = &content[3..3 + end];

        // Simple YAML to JSON conversion — handles our limited skill format
        let mut map = serde_json::Map::new();
        for line in yaml_str.lines() {
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_string();
                let value = value.trim().to_string();

                if value.starts_with('[') && value.ends_with(']') {
                    // Array value
                    let items: Vec<serde_json::Value> = value[1..value.len() - 1]
                        .split(',')
                        .map(|s| {
                            serde_json::Value::String(
                                s.trim().trim_matches('"').trim_matches('\'').to_string(),
                            )
                        })
                        .collect();
                    map.insert(key, serde_json::Value::Array(items));
                } else {
                    map.insert(key, serde_json::Value::String(value));
                }
            }
        }

        Some(serde_json::Value::Object(map))
    }

    /// Get the body text after stripping frontmatter
    fn strip_frontmatter(content: &str) -> String {
        let content = content.trim();
        if content.starts_with("---")
            && let Some(end) = content[3..].find("\n---") {
                return content[3 + end + 5..].trim().to_string();
            }
        content.to_string()
    }

    /// Check if we have any skills loaded
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// List all loaded skills
    pub fn list(&self) -> &[Skill] {
        &self.skills
    }

    /// Try to auto-create a skill from a complex interaction
    pub fn try_auto_create(&self, input: &str, response: &str) -> Result<Option<String>> {
        // This is a stub — in the real implementation, this would use the LLM
        // to analyze the interaction and suggest a skill to save.
        // For now, we log the opportunity.
        tracing::info!(
            "Auto-skill opportunity detected:\n  Input: {input:.80}\n  Response: {response:.80}"
        );
        Ok(None)
    }
}
