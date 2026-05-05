//! Model Context Protocol bridge.
//!
//! The first slice is stdio-only and opt-in: configured servers do not start
//! unless `enabled = true` and `expose_as = "auto"` are both set.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub mod client;
pub mod process;
pub mod tool_adapter;

pub use client::{McpClient, McpContent, McpTool, McpToolCallResult};
pub use process::{McpServerHandle, connect_configured_servers};
pub use tool_adapter::McpToolAdapter;

fn default_timeout_secs() -> u64 {
    30
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpExposeMode {
    Auto,
    Manual,
    #[default]
    Disabled,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub enabled: bool,
    pub expose_as: McpExposeMode,
    pub timeout_secs: u64,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            enabled: false,
            expose_as: McpExposeMode::Disabled,
            timeout_secs: default_timeout_secs(),
        }
    }
}

impl McpServerConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.max(1))
    }

    pub fn should_expose_tools(&self) -> bool {
        self.enabled && self.expose_as == McpExposeMode::Auto
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.name.trim().is_empty() {
            anyhow::bail!("MCP server name cannot be empty");
        }
        if !is_safe_identifier(&self.name) {
            anyhow::bail!(
                "MCP server `{}` has invalid name; use ASCII letters, numbers, `_`, or `-`",
                self.name
            );
        }
        if self.command.trim().is_empty() {
            anyhow::bail!("MCP server `{}` command cannot be empty", self.name);
        }
        Ok(())
    }
}

pub(crate) fn namespaced_tool_name(server: &str, tool: &str) -> String {
    format!(
        "mcp_{}_{}",
        sanitize_identifier(server),
        sanitize_identifier(tool)
    )
}

fn sanitize_identifier(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_string()
}

fn is_safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaced_tool_names_are_stable_and_safe() {
        assert_eq!(
            namespaced_tool_name("local-db", "query.table"),
            "mcp_local_db_query_table"
        );
    }
}
