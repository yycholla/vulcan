use crate::tools::ToolRegistry;

/// Builds the system prompt injected into every conversation.
pub struct PromptBuilder;

impl PromptBuilder {
    pub fn build_system_prompt(&self, tools: &ToolRegistry) -> String {
        let tool_section = format!(
            "## Available Tools\n\
             You have access to the following tools. When you need to perform an action, \
             respond with a `tool_calls` array:\n\n{}\n",
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
             When tools are needed, include a `tool_calls` block with the appropriate function calls."
        )
    }
}
