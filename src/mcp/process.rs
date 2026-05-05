use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::BufReader;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::tools::{Tool, ToolRegistry};

use super::client::McpClient;
use super::tool_adapter::McpToolAdapter;
use super::{McpServerConfig, McpTool};

type StdioClient = McpClient<BufReader<ChildStdout>, ChildStdin>;

pub struct McpServerHandle {
    name: String,
    client: Mutex<StdioClient>,
    tools: Vec<McpTool>,
    _child: Child,
    timeout: std::time::Duration,
}

impl McpServerHandle {
    pub async fn spawn(config: &McpServerConfig) -> Result<Self> {
        config.validate()?;
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .envs(&config.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start MCP server `{}`", config.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("MCP server `{}` stdout unavailable", config.name))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("MCP server `{}` stdin unavailable", config.name))?;
        let mut client = McpClient::new(BufReader::new(stdout), stdin);

        tokio::time::timeout(config.timeout(), client.initialize())
            .await
            .with_context(|| format!("MCP server `{}` initialize timed out", config.name))??;
        let tools = tokio::time::timeout(config.timeout(), client.list_tools())
            .await
            .with_context(|| format!("MCP server `{}` tools/list timed out", config.name))??;

        Ok(Self {
            name: config.name.clone(),
            client: Mutex::new(client),
            tools,
            _child: child,
            timeout: config.timeout(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    pub async fn call_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<super::McpToolCallResult> {
        let mut client = self.client.lock().await;
        tokio::time::timeout(self.timeout, client.call_tool(tool_name, arguments))
            .await
            .with_context(|| format!("MCP tool `{}`.`{tool_name}` timed out", self.name))?
    }
}

pub async fn connect_configured_servers(
    configs: &[McpServerConfig],
    registry: &mut ToolRegistry,
) -> Vec<Arc<McpServerHandle>> {
    let mut handles = Vec::new();
    for config in configs {
        if !config.should_expose_tools() {
            continue;
        }
        match McpServerHandle::spawn(config).await {
            Ok(handle) => {
                let handle = Arc::new(handle);
                for tool in handle.tools() {
                    let adapter = Arc::new(McpToolAdapter::new(Arc::clone(&handle), tool.clone()));
                    if registry.contains(adapter.name()) {
                        tracing::warn!(
                            server = handle.name(),
                            tool = adapter.name(),
                            "MCP tool conflicts with existing registry tool; skipping"
                        );
                        continue;
                    }
                    registry.register(adapter);
                }
                handles.push(handle);
            }
            Err(err) => {
                tracing::warn!(
                    server = config.name.as_str(),
                    %err,
                    "MCP server unavailable; skipping tool exposure"
                );
            }
        }
    }
    handles
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::McpExposeMode;
    use std::collections::HashMap;

    #[tokio::test]
    async fn failed_server_startup_has_actionable_error() {
        let config = McpServerConfig {
            name: "missing".to_string(),
            command: "vulcan-missing-mcp-server-command-for-test".to_string(),
            args: Vec::new(),
            env: HashMap::new(),
            enabled: true,
            expose_as: McpExposeMode::Auto,
            timeout_secs: 1,
        };

        let err = match McpServerHandle::spawn(&config).await {
            Ok(_) => panic!("expected missing command to fail"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(msg.contains("failed to start MCP server `missing`"));
    }
}
