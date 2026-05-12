use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::code::{Language, ParserCache};
use crate::config::CodeOutlineAssistConfig;
use crate::provider::Message;
use crate::tools::code::outline;

use super::{HookHandler, HookOutcome, InjectPosition};

pub struct CodeOutlineHook {
    workspace: PathBuf,
    config: CodeOutlineAssistConfig,
    cache: Arc<ParserCache>,
}

impl CodeOutlineHook {
    pub fn new(workspace: PathBuf, config: CodeOutlineAssistConfig) -> Self {
        let workspace = workspace.canonicalize().unwrap_or(workspace);
        Self {
            workspace,
            config,
            cache: Arc::new(ParserCache::new()),
        }
    }

    fn latest_user_message<'a>(&self, messages: &'a [Message]) -> Option<&'a str> {
        messages.iter().rev().find_map(|message| match message {
            Message::User { content } => Some(content.as_str()),
            _ => None,
        })
    }

    fn candidate_paths(&self, prompt: &str) -> Vec<PathBuf> {
        let workspace = self
            .workspace
            .canonicalize()
            .unwrap_or_else(|_| self.workspace.clone());
        let mut seen = HashSet::new();
        let mut out = Vec::new();

        for raw in prompt.split_whitespace() {
            if out.len() >= self.config.max_files {
                break;
            }
            let token = clean_path_token(raw);
            if token.is_empty() {
                continue;
            }
            let path = PathBuf::from(&token);
            if Language::from_path(&path).is_none() {
                continue;
            }

            let candidate = if path.is_absolute() {
                path
            } else {
                workspace.join(path)
            };
            let Ok(canonical) = candidate.canonicalize() else {
                continue;
            };
            if !canonical.starts_with(&workspace) || !canonical.is_file() {
                continue;
            }
            if seen.insert(canonical.clone()) {
                out.push(canonical);
            }
        }

        out
    }

    async fn render_outline(&self, path: &Path) -> Result<Option<String>> {
        let Some(lang) = Language::from_path(path) else {
            return Ok(None);
        };
        let source = tokio::fs::read_to_string(path).await?;
        let mut symbols = outline(&self.cache, lang, &source)?;
        if symbols.is_empty() {
            return Ok(None);
        }
        let truncated = symbols.len() > self.config.max_symbols_per_file;
        symbols.truncate(self.config.max_symbols_per_file);

        let display_path = path
            .strip_prefix(&self.workspace)
            .unwrap_or(path)
            .display()
            .to_string();
        let mut section = format!("### {display_path} ({})\n", lang.name());
        for symbol in symbols {
            section.push_str(&format!(
                "- {} {}: lines {}-{}\n",
                symbol.kind, symbol.name, symbol.start_line, symbol.end_line
            ));
        }
        if truncated {
            section.push_str(&format!(
                "- … truncated to {} symbols\n",
                self.config.max_symbols_per_file
            ));
        }
        Ok(Some(section))
    }
}

#[async_trait]
impl HookHandler for CodeOutlineHook {
    fn name(&self) -> &str {
        "code_outline_assist"
    }

    fn priority(&self) -> i32 {
        20
    }

    async fn before_prompt(
        &self,
        messages: &[Message],
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        if !self.config.enabled || self.config.max_files == 0 || self.config.max_prompt_chars == 0 {
            return Ok(HookOutcome::Continue);
        }

        let Some(prompt) = self.latest_user_message(messages) else {
            return Ok(HookOutcome::Continue);
        };
        let paths = self.candidate_paths(prompt);
        if paths.is_empty() {
            return Ok(HookOutcome::Continue);
        }

        let mut body = String::from(
            "## Code Outline Assist\nStructural outlines for source files mentioned in the latest user prompt. These are symbol names and line ranges only; source bodies were not injected.\n",
        );
        for path in paths {
            match self.render_outline(&path).await {
                Ok(Some(section)) => {
                    if body.len() + section.len() > self.config.max_prompt_chars {
                        body.push_str("\n… outline assist truncated by max_prompt_chars\n");
                        break;
                    }
                    body.push('\n');
                    body.push_str(&section);
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::debug!(path = %path.display(), error = %err, "code outline assist skipped file");
                }
            }
        }

        if !body.contains("### ") {
            return Ok(HookOutcome::Continue);
        }

        Ok(HookOutcome::InjectMessages {
            messages: vec![Message::System { content: body }],
            position: InjectPosition::AfterSystem,
        })
    }
}

fn clean_path_token(raw: &str) -> String {
    raw.trim_matches(|c: char| {
        matches!(
            c,
            '`' | '\'' | '"' | ',' | ';' | ':' | ')' | '(' | '[' | ']' | '{' | '}' | '<' | '>'
        )
    })
    .trim_end_matches('.')
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::{HookHandler, HookOutcome, InjectPosition};
    use crate::provider::Message;
    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn disabled_hook_does_not_inject() {
        let dir = tempdir().unwrap();
        let hook = CodeOutlineHook::new(
            dir.path().to_path_buf(),
            CodeOutlineAssistConfig {
                enabled: false,
                ..CodeOutlineAssistConfig::default()
            },
        );

        let outcome = hook
            .before_prompt(
                &[Message::User {
                    content: "Please update src/lib.rs".into(),
                }],
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(matches!(outcome, HookOutcome::Continue));
    }

    #[tokio::test]
    async fn code_prompt_injects_bounded_outline_for_mentioned_workspace_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("lib.rs");
        std::fs::write(
            &path,
            "fn alpha() {}\nstruct Beta { x: i32 }\nfn gamma() {}\n",
        )
        .unwrap();
        let hook = CodeOutlineHook::new(
            dir.path().to_path_buf(),
            CodeOutlineAssistConfig {
                enabled: true,
                max_files: 2,
                max_symbols_per_file: 2,
                max_prompt_chars: 2_000,
            },
        );

        let outcome = hook
            .before_prompt(
                &[Message::User {
                    content: "Can you refactor lib.rs without reading the whole file?".into(),
                }],
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let HookOutcome::InjectMessages { messages, position } = outcome else {
            panic!("expected outline injection, got {outcome:?}");
        };
        assert_eq!(position, InjectPosition::AfterSystem);
        assert_eq!(messages.len(), 1);
        let Message::System { content } = &messages[0] else {
            panic!("expected system message");
        };
        assert!(content.contains("## Code Outline Assist"), "{content}");
        assert!(content.contains("lib.rs (rust)"), "{content}");
        assert!(content.contains("- function alpha: lines 1-1"), "{content}");
        assert!(content.contains("- struct Beta: lines 2-2"), "{content}");
        assert!(content.contains("truncated"), "{content}");
        assert!(
            !content.contains("fn alpha"),
            "should not dump source: {content}"
        );
    }

    #[tokio::test]
    async fn ignores_paths_outside_workspace() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("secret.rs");
        std::fs::write(&outside_file, "fn secret() {}\n").unwrap();
        let hook = CodeOutlineHook::new(
            dir.path().to_path_buf(),
            CodeOutlineAssistConfig {
                enabled: true,
                ..CodeOutlineAssistConfig::default()
            },
        );

        let outcome = hook
            .before_prompt(
                &[Message::User {
                    content: format!("summarize {}", outside_file.display()),
                }],
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(matches!(outcome, HookOutcome::Continue));
    }
}
