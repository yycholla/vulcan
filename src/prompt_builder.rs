use crate::tools::{ToolContext, ToolRegistry};

/// Builds the system prompt injected into every conversation.
pub struct PromptBuilder;

impl PromptBuilder {
    pub fn build_system_prompt(&self, tools: &ToolRegistry) -> String {
        self.build_system_prompt_with_context(tools, None)
    }

    pub fn build_system_prompt_with_context(
        &self,
        tools: &ToolRegistry,
        ctx: Option<&ToolContext>,
    ) -> String {
        let cwd = ctx
            .map(|c| c.cwd.display().to_string())
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|p| p.display().to_string())
            })
            .unwrap_or_else(|| "(unknown)".into());
        let mut env_lines = vec![format!("- working directory: `{cwd}`")];
        if let Some(c) = ctx {
            if let Some(manifest) = &c.cargo_manifest {
                let pkg = c
                    .cargo_package_name
                    .clone()
                    .unwrap_or_else(|| "(unnamed)".into());
                env_lines.push(format!(
                    "- rust workspace: package `{pkg}` at `{}`",
                    manifest.display()
                ));
                if !c.cargo_bin_targets.is_empty() {
                    env_lines.push(format!("- bin targets: {}", c.cargo_bin_targets.join(", ")));
                }
            } else {
                env_lines.push(
                    "- no Cargo.toml within depth=4 — `cargo_check` is not registered".into(),
                );
            }
            if c.git_present {
                env_lines.push("- git: working tree present".into());
            }
        }
        env_lines.push(
            "- bash and other shell commands run in the working directory unless `workdir` is supplied.".into(),
        );
        let env_section = format!("## Environment\n{}\n", env_lines.join("\n"));

        let tool_section = format!(
            "## Available Tools\n\
             You have access to the following tools. When you need to perform an action, \
             call the appropriate tool directly through the tool-calling interface:\n\n{}\n",
            tools
                .definitions_with_context(ctx)
                .iter()
                .map(|t| format!("- `{}`: {}", t.function.name, t.function.description))
                .collect::<Vec<_>>()
                .join("\n")
        );

        let preferences_section = "## Tool preferences\n\
             Prefer native tools over `bash`. Bash is the last resort — use it only \
             for pipes, ad-hoc sysadmin, or commands no native tool covers.\n\n\
             | Task | Use | Not |\n\
             |---|---|---|\n\
             | Search file contents | `search_files` | `bash rg` / `grep -r` |\n\
             | Structural code query | `code_query` (tree-sitter) | `bash grep` |\n\
             | Read a file | `read_file` | `bash cat` / `head` / `tail` |\n\
             | List a directory | `list_files` | `bash ls` / `tree` / `find` |\n\
             | Check Rust compiles | `cargo_check` | `bash cargo check` |\n\
             | Git status / diff / log / commit / push | `git_*` | `bash git ...` |\n\
             | Goto-def, refs, hover | `goto_definition` / `find_references` / `hover` | (no bash equiv) |\n";

        format!(
            "You are Vulcan, a Rust AI agent. You help with coding, research, \
             file management, and automation.\n\n\
             {env_section}\n\
             ## Guidelines\n\
             - Be concise and precise\n\
             - When working on multi-step tasks, use tools iteratively — one tool call at a time\n\
             - Read files before editing them\n\
             - After 5+ tool calls on a complex task, suggest saving a skill\n\n\
             {preferences_section}\n\
             {tool_section}\n\
             ## Response Format\n\
             Respond naturally. If the user's request doesn't need tools, just answer directly. \
             When tools are needed, call the tool directly instead of printing a JSON blob or a literal \
             `tool_calls` block in assistant text. Tool call arguments must be valid JSON matching the tool schema."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_mentions_json_for_tool_call_mode() {
        let prompt = PromptBuilder.build_system_prompt(&ToolRegistry::new());
        assert!(
            prompt.to_lowercase().contains("json"),
            "prompt must mention json for providers that require it when json_object mode is enabled: {prompt:?}"
        );
    }

    #[test]
    fn system_prompt_does_not_tell_models_to_print_literal_tool_calls_blocks() {
        let prompt = PromptBuilder.build_system_prompt(&ToolRegistry::new());
        assert!(
            !prompt.contains("include a `tool_calls` block"),
            "prompt should instruct native tool calling, not literal tool_calls text output: {prompt:?}"
        );
    }

    /// Snapshot guard for the system prompt produced against the *default*
    /// tool registry. Catches accidental wording drift in the
    /// guidelines/preferences/format sections without rewriting the
    /// existing per-keyword assertions. Tool list is normalized so adding
    /// a new tool doesn't churn this snapshot.
    #[test]
    fn system_prompt_snapshot_default_registry() {
        let prompt = PromptBuilder.build_system_prompt(&ToolRegistry::new());
        let normalized = normalize_tool_list(&prompt);
        insta::assert_snapshot!("system_prompt_default_registry", normalized);
    }

    fn normalize_tool_list(prompt: &str) -> String {
        // Replace the `## Available Tools` body up to the next `##` with a
        // single placeholder so prompt copy changes are caught here, but
        // tool churn lives in dedicated assertions above.
        let marker = "## Available Tools";
        let mut out = String::with_capacity(prompt.len());
        if let Some(start) = prompt.find(marker) {
            let tail = &prompt[start + marker.len()..];
            let end_rel = tail.find("\n##").unwrap_or(tail.len());
            out.push_str(&prompt[..start]);
            out.push_str(marker);
            out.push_str("\n<tool list normalized>\n");
            out.push_str(&tail[end_rel..]);
        } else {
            out.push_str(prompt);
        }
        // Snapshot guard runs in CI (different absolute path) and on the
        // contributor's box. Strip the working-directory line so the
        // snapshot pins prompt copy, not the runner's filesystem.
        normalize_cwd_line(&out)
    }

    fn normalize_cwd_line(prompt: &str) -> String {
        let mut out = String::with_capacity(prompt.len());
        for line in prompt.split_inclusive('\n') {
            let trimmed = line.trim_start();
            if trimmed.starts_with("- working directory:") {
                let leading_ws = &line[..line.len() - trimmed.len()];
                out.push_str(leading_ws);
                out.push_str("- working directory: <CWD>");
                // Preserve original line ending
                if line.ends_with('\n') {
                    out.push('\n');
                }
            } else {
                out.push_str(line);
            }
        }
        out
    }

    /// YYC-86: the prompt must steer the model toward native tools and
    /// position bash as a last resort. Without this section, even with
    /// the YYC-85 description nudges the agent still falls back to
    /// `rg`/`cat`/`cargo check` via bash.
    #[test]
    fn system_prompt_includes_native_tool_preference_table() {
        let prompt = PromptBuilder.build_system_prompt(&ToolRegistry::new());
        assert!(
            prompt.contains("Tool preferences"),
            "prompt missing 'Tool preferences' section: {prompt:?}"
        );
        for native in [
            "search_files",
            "cargo_check",
            "git_status",
            "read_file",
            "list_files",
        ] {
            assert!(
                prompt.contains(native),
                "preference table missing `{native}` reference: {prompt:?}"
            );
        }
        assert!(
            prompt.to_lowercase().contains("last resort"),
            "prompt should call bash a last resort: {prompt:?}"
        );
    }
}
