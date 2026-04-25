use crate::provider::ToolDefinition;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Canonical tool return type — the wire format between `Tool::call`, the
/// agent loop, and `AfterToolCall` hooks.
///
/// `output` goes to the LLM (via `Message::Tool` content). `media` carries
/// file paths for attachments (images, audio, etc.) — the agent serializes
/// them inline as `[media: ...]` markers when flattening for the message
/// payload, but hooks and the TUI see them as a separate field. `is_error`
/// is the structured signal that something went wrong (preferred over
/// string-prefix sniffing like `output.starts_with("Error:")`).
#[derive(Debug, Clone, Default)]
pub struct ToolResult {
    pub output: String,
    pub media: Vec<String>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            media: Vec::new(),
            is_error: false,
        }
    }

    pub fn err(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            media: Vec::new(),
            is_error: true,
        }
    }
}

impl From<String> for ToolResult {
    fn from(output: String) -> Self {
        Self::ok(output)
    }
}

impl From<&str> for ToolResult {
    fn from(output: &str) -> Self {
        Self::ok(output)
    }
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> Value;
    async fn call(&self, params: Value) -> Result<ToolResult>;
}

pub mod file;
pub mod shell;
pub mod web;

/// Registry of available tools — tools are discovered at startup via the `inventory` pattern
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            tools: HashMap::new(),
        };
        registry.register(Arc::new(file::ReadFile));
        registry.register(Arc::new(file::WriteFile));
        registry.register(Arc::new(file::SearchFiles));
        registry.register(Arc::new(file::PatchFile));
        registry.register(Arc::new(web::WebSearch));
        registry.register(Arc::new(web::WebFetch));
        registry.register(Arc::new(shell::BashTool));
        registry
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Get all tool definitions for the LLM
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|t| ToolDefinition {
                tool_type: "function".into(),
                function: crate::provider::ToolFunction {
                    name: t.name().to_string(),
                    description: t.description().to_string(),
                    parameters: t.schema(),
                },
            })
            .collect()
    }

    /// Execute a tool by name with JSON arguments.
    pub async fn execute(&self, name: &str, arguments: &str) -> Result<ToolResult> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Unknown tool: {name}"))?;

        let params: Value = serde_json::from_str(arguments)
            .map_err(|e| anyhow::anyhow!("Failed to parse arguments for {name}: {e}"))?;

        tool.call(params).await
    }
}
