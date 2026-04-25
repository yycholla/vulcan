use crate::tools::ToolRegistry;

/// Builds the system prompt injected into every conversation.
pub struct PromptBuilder;

impl PromptBuilder {
    pub fn build_system_prompt(&self, tools: &ToolRegistry) -> String {
        let tool_section = format!(
            "## Available Tools\n\
             You have access to the following tools. When you need to perform an action, \
             call the appropriate tool directly through the tool-calling interface:\n\n{}\n",
            tools
                .definitions()
                .iter()
                .map(|t| format!("- `{}`: {}", t.function.name, t.function.description))
                .collect::<Vec<_>>()
                .join("\n")
        );

        format!(
            "You are Vulcan, a Rust AI agent. You help with coding, research, \
             file management, and automation.\n\n\
             ## Guidelines\n\
             - Be concise and precise\n\
             - When working on multi-step tasks, use tools iteratively — one tool call at a time\n\
             - Read files before editing them\n\
             - Use `bash` for builds, installs, git operations, and scripts\n\
             - After 5+ tool calls on a complex task, suggest saving a skill\n\n\
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
}
