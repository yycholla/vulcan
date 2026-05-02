---
title: MCP Server Support
type: feature
created: 2026-05-14
tags: [extensions, mcp, llm-tools, interoperability]
---

# MCP Server Support

First-class support for the **Model Context Protocol (MCP)** — enabling extensions and agents to seamlessly integrate with external data sources, tools, and services through a standardized, secure protocol.

## What is MCP?

MCP provides a standardized way for LLM applications to expose and consume tools, resources, and prompts from external servers. Rather than hard-coding integrations, agents can dynamically discover and use capabilities exposed by MCP servers over stdio or transport layers.

## Architecture

### MCP Client (Built-in)

Vulcan embeds an MCP client that can connect to one or more MCP servers on behalf of extensions or the agent itself.

- **Transport**: stdio (default), SSE (Server-Sent Events), or in-process pipes for local servers
- **Lifecycle**: Start/stop servers, monitor health, auto-restart on crash
- **Security**: Per-server permission scopes, filesystem access limits, network allowlists/denylists

### MCP ↔ Extension Bridge

Extensions can declare dependency on MCP capabilities. The bridge translates between MCP primitives and Vulcan-native concepts:

| MCP Concept | Vulcan Equivalent | Notes |
|-------------|-------------------|-------|
| `tool/*`    | `Capability::ToolProvider` | Exposed as native tools |
| `resource/*`| MemoryBackend / read-only store | Queried via tools/actions |
| `prompt/*`  | Injectable prompt fragments | Rendered in-context |
| `sampling`  | Agent delegation / sub-agent | LLM-in-the-loop recursion |

### Two Integration Modes

#### 1. Transparent Mode (Auto-Expose as Tools)

MCP servers are started alongside the agent; all declared tools are automatically registered in the tool registry and appear to the agent as native capabilities.

```toml
# ~/.vulcan/config.toml
[[mcp_servers]]
name = "postgres"
command = "uvx"
args = ["mcp-server-postgres", "--db-url", "postgres://localhost/mydb"]
env = { "PGPASSWORD" = "vault://pg/password" }
expose_as = "auto"          # register as native tools
permissions = { network = "localhost:5432", filesystem = "none" }
```

Result: `list_tables`, `query`, `describe_schema` appear as agent tools immediately.

#### 2. Bridged Mode (Extension-Controlled)

Extensions explicitly connect to MCP servers and interpret their capabilities programmatically, allowing richer behaviors (batching, caching, stateful sessions).

```rust
pub struct McpBackedExtension {
    client: McpClient,
}

impl Extension for McpBackedExtension {
    fn initialize(&self, ctx: &ExtensionContext) -> Result<()> {
        // Connect to a specific server
        let server = self.client.connect("memory-server", Transport::Stdio("mcp-memory-server"))?;

        // Discover available tools
        let tools = server.list_tools()?;

        // Wrap each MCP tool with policy, logging, and retry logic
        for t in tools {
            let wrapped = McpToolAdapter::new(server.clone(), t)
                .with_rate_limit(100, Duration::from_secs(60))
                .with_cache(Duration::from_secs(300));
            ctx.register_tool(wrapped.name(), Arc::new(wrapped));
        }
        Ok(())
    }
}
```

## Key Features

### Secure Server Execution

- **Sandboxed stdio**: Servers run as child processes with configurable resource limits (RLIMIT, cgroups on Linux).
- **Network policies**: Restrict servers to specific hosts/ports; deny by default.
- **Filesystem scoping**: Root directory jails for file-capable servers.
- **Secret injection**: Secrets provided via env or vault, never hard-coded.

### Capability Negotiation

On connect, the client queries server capabilities (`initialize` + `list_tools/resources/prompts`) and validates against extension requirements and policy. Mismatches fail fast with clear diagnostics.

### Sampling Support

MCP servers can request LLM sampling (i.e., recursion) via `sampling/createMessage`. The bridge can:
- Forward to the parent agent (with depth limits)
- Spawn a sub-agent with bounded budget
- Deny and return error if not permitted

This enables servers to perform multi-step reasoning using tools they don't possess locally.

### Resource Templates

MCP resource templates allow dynamic URI spaces (e.g., `postgres://host/db/schemas/{schema}/tables/{table}`). Extensions can iterate these to auto-generate browsing tools.

## CLI & DevEx

```bash
# List running MCP servers and their capabilities
vulcan mcp list

# Start a server in foreground (debugging)
vulcan mcp start --name pg-explorer --stdio ...

# Test an MCP tool manually
vulcan mcp call pg-explorer list_tables

# Validate server permissions against policy
vulcan mcp check-permissions --config my-server.json
```

## Example: Slack MCP Integration

```toml
# ~/.vulcan/config.toml
[[mcp_servers]]
name = "slack"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-slack", "--token", "xoxb-..."]
expose_as = "auto"
permissions = { network = "slack.com" }
```

The agent can now:
- `slack_list_channels`
- `slack_read_messages` (channel ID required)
- `slack_post_message` (with approval for production channels)

All surfaced as normal tools, with audit logging and policy checks applied.

## Example: PostgreSQL MCP + Extension

A security-focused extension wraps the PostgreSQL MCP server to add row-level security and safe query construction:

```rust
pub struct SafeQueryExtension {
    mcp: McpClient,
}

impl Extension for SafeQueryExtension {
    fn initialize(&self, ctx: &ExtensionContext) -> Result<()> {
        let pg = self.mcp.connect("postgres")?;
        ctx.register_tool(Arc::new(SafeSelectTool { pg }));
        Ok(())
    }
}

struct SafeSelectTool { pg: McpClient }

impl Tool for SafeSelectTool {
    fn call(&self, args: Value) -> Result<Value> {
        // Only allow SELECT, enforce tenant_id filter, time-box query
        validate_select_only(&args)?;
        self.pg.call_tool("execute_query", sanitized(args))
    }
}
```

## Comparison: Extensions vs MCP Servers

| | Native Extensions | MCP Servers |
|---|---|---|
| Language | Rust (compiled) / WASM / Script | Any (Python, JS, Go, etc.) — anything that speaks MCP |
| Performance | High | IPC overhead (stdio) / network (SSE) |
| Isolation | Process/WASM/container boundary | Process boundary |
| Integration depth | Full access to Vulcan APIs | Protocol-limited (tools, resources, prompts, sampling) |
| Development effort | Higher (Rust, compiles) | Lower (rapid scripting) |
| Security surface | Larger (native code) | Smaller (protocol boundary, easier auditing) |

Use MCP for quick integrations and scripting-friendly capabilities; use native extensions for high-performance, deep platform features.

## Future

- **MCP-aware registry**: Discover and install MCP servers from the extension store as managed dependencies.
- **Bidirectional MCP**: Vulcan itself can act as an MCP server, allowing external IDEs/agents to query agent state and tools.
- **Streaming resources**: Long-lived resource subscriptions (e.g., live logs, metrics) via MCP streams.
