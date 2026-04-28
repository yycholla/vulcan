//! YYC-225 (YYC-165 PR-2): parse optional `extension:` metadata
//! out of skill markdown frontmatter.
//!
//! The existing skill loader keeps doing its thing; this module
//! is a *peek* path that pulls extra metadata out without
//!   altering the parsed `Skill` value. Skills without an
//! `extension:` block produce `None` here, so backward
//! compatibility is by construction.
//!
//! The resulting `ExtensionMetadata` always has `status =
//! Draft`. Activation is intentionally impossible at this layer —
//! drafts only surface in the registry; PR-4 lands code-backed
//! activation by separate API.

use super::{ExtensionCapability, ExtensionMetadata, ExtensionSource, ExtensionStatus};

/// Parse the frontmatter of a skill markdown file and extract
/// an optional `extension:` declaration. Returns `None` when:
///
/// - the file has no `---`-delimited frontmatter,
/// - the frontmatter has no top-level `extension:` key, or
/// - the `extension:` block is missing the required `id` field.
///
/// The parser is intentionally narrow — it understands a single
/// indented YAML-ish block and the keys we care about. Unknown
/// keys are ignored so future fields land without breaking
/// anything.
pub fn parse_skill_extension(content: &str) -> Option<ExtensionMetadata> {
    let frontmatter = extract_frontmatter(content)?;
    let block = extract_indented_block(&frontmatter, "extension")?;

    let mut id: Option<String> = None;
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut description: Option<String> = None;
    let mut capabilities: Vec<ExtensionCapability> = Vec::new();
    let mut permissions: Option<String> = None;
    let mut priority: Option<i32> = None;

    for line in block.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (key, value) = match trimmed.split_once(':') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => continue,
        };
        let value = strip_quotes(value);
        match key {
            "id" => id = Some(value.to_string()),
            "name" => name = Some(value.to_string()),
            "version" => version = Some(value.to_string()),
            "description" => description = Some(value.to_string()),
            "permissions" => permissions = Some(value.to_string()),
            "priority" => priority = value.parse::<i32>().ok(),
            "capabilities" => {
                if value.starts_with('[') && value.ends_with(']') {
                    capabilities.extend(parse_capability_list(&value[1..value.len() - 1]));
                }
            }
            _ => {}
        }
    }

    let id = id?;
    let name = name.unwrap_or_else(|| id.clone());
    let version = version.unwrap_or_else(|| "0.0.0".to_string());

    let mut metadata = ExtensionMetadata::new(&id, &name, &version, ExtensionSource::SkillDraft);
    metadata.status = ExtensionStatus::Draft;
    if let Some(desc) = description {
        metadata.description = desc;
    }
    metadata.capabilities = capabilities;
    metadata.permissions_summary = permissions;
    if let Some(p) = priority {
        metadata.priority = p;
    }
    Some(metadata)
}

fn extract_frontmatter(content: &str) -> Option<&str> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let after = &trimmed[3..];
    let end = after.find("\n---")?;
    Some(&after[..end])
}

/// Pull a single top-level YAML key whose value is an indented
/// block (`key:\n  sub: value`). Returns the de-indented body
/// or `None` when the key is missing or has an inline scalar.
fn extract_indented_block<'a>(frontmatter: &'a str, key: &str) -> Option<String> {
    let mut lines = frontmatter.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_end();
        // We want the line to look exactly like `key:` (optional
        // trailing whitespace). Inline scalars (`key: value`) are
        // ignored — they wouldn't be a block anyway.
        if trimmed.trim_start() == format!("{key}:") {
            // Capture all subsequent indented lines.
            let mut block = String::new();
            while let Some(next) = lines.peek() {
                if next.starts_with(' ') || next.starts_with('\t') {
                    block.push_str(next.trim_start());
                    block.push('\n');
                    lines.next();
                } else if next.trim().is_empty() {
                    block.push('\n');
                    lines.next();
                } else {
                    break;
                }
            }
            if block.is_empty() {
                return None;
            }
            return Some(block);
        }
    }
    None
}

fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    s.trim_matches('"').trim_matches('\'')
}

fn parse_capability_list(raw: &str) -> Vec<ExtensionCapability> {
    raw.split(',')
        .map(|s| strip_quotes(s.trim()))
        .filter_map(parse_capability)
        .collect()
}

fn parse_capability(raw: &str) -> Option<ExtensionCapability> {
    match raw {
        "prompt_injection" => Some(ExtensionCapability::PromptInjection),
        "hook_handler" => Some(ExtensionCapability::HookHandler),
        "tool_provider" => Some(ExtensionCapability::ToolProvider),
        "memory_backend" => Some(ExtensionCapability::MemoryBackend),
        "lifecycle_observer" => Some(ExtensionCapability::LifecycleObserver),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_without_extension_block_returns_none() {
        let content = r#"---
name: hello
description: greet the user
triggers: ["hi"]
---
# Hello skill

Body text.
"#;
        assert!(parse_skill_extension(content).is_none());
    }

    #[test]
    fn malformed_frontmatter_returns_none() {
        // No closing ---.
        let content = "---\nname: hello\n# missing close\n";
        assert!(parse_skill_extension(content).is_none());
    }

    #[test]
    fn extension_block_returns_draft_metadata() {
        let content = r#"---
name: lint-helper
description: helper skill
extension:
  id: lint-helper
  name: Lint Helper
  version: 0.2.0
  description: surfaces clippy notes proactively
  capabilities: [prompt_injection, hook_handler]
  permissions: read-only file access
  priority: 50
---
# Lint Helper
"#;
        let meta = parse_skill_extension(content).expect("draft parses");
        assert_eq!(meta.id, "lint-helper");
        assert_eq!(meta.name, "Lint Helper");
        assert_eq!(meta.version, "0.2.0");
        assert_eq!(meta.status, ExtensionStatus::Draft);
        assert_eq!(meta.source, ExtensionSource::SkillDraft);
        assert_eq!(meta.priority, 50);
        assert_eq!(
            meta.capabilities,
            vec![
                ExtensionCapability::PromptInjection,
                ExtensionCapability::HookHandler
            ]
        );
        assert_eq!(
            meta.permissions_summary.as_deref(),
            Some("read-only file access")
        );
    }

    #[test]
    fn extension_block_without_id_returns_none() {
        let content = r#"---
name: nameless
extension:
  version: 0.1.0
---
"#;
        // id is required.
        assert!(parse_skill_extension(content).is_none());
    }

    #[test]
    fn unknown_capability_strings_are_dropped() {
        let content = r#"---
extension:
  id: weird
  capabilities: [prompt_injection, telepathy, hook_handler]
---
"#;
        let meta = parse_skill_extension(content).unwrap();
        assert_eq!(
            meta.capabilities,
            vec![
                ExtensionCapability::PromptInjection,
                ExtensionCapability::HookHandler,
            ]
        );
    }

    #[test]
    fn drafts_added_to_registry_remain_inactive() {
        // YYC-225 acceptance pin: parsing a draft from skill
        // frontmatter must NEVER mark it Active. Activation
        // arrives in PR-4 via a separate API.
        let content = r#"---
extension:
  id: secret-tool
  capabilities: [tool_provider]
---
"#;
        let meta = parse_skill_extension(content).unwrap();
        let reg = crate::extensions::ExtensionRegistry::new();
        reg.upsert(meta);
        let listed = reg.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, ExtensionStatus::Draft);
        // No Active extensions with ToolProvider capability —
        // the gating contract.
        assert!(
            reg.active_with_capability(ExtensionCapability::ToolProvider)
                .is_empty(),
            "drafts must not surface as active capability providers"
        );
    }

    #[test]
    fn defaults_apply_when_optional_fields_omitted() {
        let content = r#"---
extension:
  id: minimal
---
"#;
        let meta = parse_skill_extension(content).unwrap();
        assert_eq!(meta.id, "minimal");
        assert_eq!(meta.name, "minimal"); // fallback to id
        assert_eq!(meta.version, "0.0.0"); // default
        assert_eq!(meta.priority, 100); // default from `new`
        assert!(meta.capabilities.is_empty());
        assert_eq!(meta.status, ExtensionStatus::Draft);
    }
}
