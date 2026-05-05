use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::tools::{ProgressSink, ReplaySafety, Tool, ToolResult};

use super::{McpServerHandle, McpTool, namespaced_tool_name};

pub struct McpToolAdapter {
    name: String,
    server_tool_name: String,
    description: String,
    schema: Value,
    server: Arc<McpServerHandle>,
}

impl McpToolAdapter {
    pub fn new(server: Arc<McpServerHandle>, tool: McpTool) -> Self {
        let name = namespaced_tool_name(server.name(), &tool.name);
        let description = tool.description.unwrap_or_else(|| {
            format!(
                "Call MCP tool `{}` from configured MCP server `{}`.",
                tool.name,
                server.name()
            )
        });
        let schema = if tool.input_schema.is_null() {
            json!({"type": "object", "properties": {}})
        } else {
            tool.input_schema
        };
        Self {
            name,
            server_tool_name: tool.name,
            description,
            schema,
            server,
        }
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn schema(&self) -> Value {
        self.schema.clone()
    }

    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::External
    }

    async fn call(
        &self,
        params: Value,
        cancel: CancellationToken,
        _progress: Option<ProgressSink>,
    ) -> Result<ToolResult> {
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(ToolResult::err("Cancelled")),
            result = self.server.call_tool(&self.server_tool_name, params) => result?,
        };

        let output = result
            .content
            .iter()
            .filter_map(|part| {
                if part.content_type == "text" {
                    part.text.clone()
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let output = if output.is_empty() {
            serde_json::to_string_pretty(&result)?
        } else {
            output
        };

        let details = serde_json::to_value(&result)?;
        let tool_result = if result.is_error {
            ToolResult::err(output)
        } else {
            ToolResult::ok(output)
        };
        Ok(tool_result.with_details(details))
    }
}
