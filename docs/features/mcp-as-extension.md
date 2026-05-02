---
title: MCP as an Extension
type: promotion
created: 2026-05-14
tags: [extensions, mcp, promotion, bridge]
---

# MCP as an Extension

MCP integration can be promoted from a standalone protocol feature into a first-class extension — and optionally into a managed extension that embeds and governs MCP servers themselves.

There are two complementary promotion paths:

- **MCP Client/Bridge Extension** — the agent embeds a robust MCP client that discovers, connects to, and governs remote MCP servers; translates MCP primitives into native tools/resources; and enforces policy, caching, and retry semantics.
- **MCP Server Hosting Extension** — the agent (or an extension) starts, stops, and monitors local MCP servers as managed child processes (or WASM hosts) and publishes their capabilities into the tool registry under policy controls.

Both paths naturally follow the skill → draft extension → code extension promotion ladder.

---

## 1. Skill (Markdown) — "How to use MCP with Vulcan"

A markdown skill lives in `~/.vulcan/skills/` and gives humans instructions and ad-hoc commands.

```markdown
---
name: mcp-integration
description: Use Model Context Protocol servers as tools and resources
triggers: ["mcp", "use mcp", "add mcp server"]
---

## Instructions

1. Install an MCP server (e.g. `npm i -g @modelcontextprotocol/server-filesystem`).
2. Add it to `~/.vulcan/config.toml` under `[[mcp_servers]]`.
3. Start Vulcan; the built-in MCP client will connect and list available tools.
4. Call tools like `mcp_tool(<server>, <tool_name>, ...)`.

## Promotion hints

This skill is a candidate for promotion because:
- It is frequently used across sessions.
- It involves repeated configuration and safety checks.
- It benefits from caching, structured tool registration, and policy enforcement.
```

---

## 2. Draft Extension — Structured configuration + policy hints

Promote the skill to a draft extension by adding richer frontmatter and making capabilities explicit.

```markdown
---
name: mcp-bridge
description: Bridge MCP servers into native tools and resources
extension: candidate
extension_confidence: 0.85
triggers: ["mcp", "mcp servers", "add server"]
config_schema:
  type: object
  properties:
    servers:
      type: array
      items:
        type: object
        properties:
          name: { type: string }
          command: { type: string }
          args: { type: array; items: { type: string } }
          env: { type: object }
          permissions:
            type: object
            properties:
              network: { type: string }
              filesystem: { type: string }
          expose_as: { type: string; enum: ["auto", "manual", "disabled"] }
depends:
  - stdio
  - network
  - process
---

The MCP Bridge extension can:
- Start MCP servers on demand (stdio) or connect to remote SSE endpoints.
- Negotiate capabilities and validate them against policy before registering tools.
- Wrap MCP tools with retry, timeouts, and call logging.
```

This draft enables richer UI (server status, per-server enable/disable) and structured validation of `mcp_servers` config.

---

## 3. Code Extension — Native MCP Bridge

A code-backed `McpBridgeExtension` implements the client, lifecycle, and integration with Vulcan internals.

### Responsibilities

- **Discovery**: Query each configured server via `initialize` and `list_tools/resources/prompts`.
- **Lifecycle**: Start/stop stdio servers, monitor health, restart on crash (with backoff).
- **Capability negotiation**: Map MCP tools to `Capability::ToolProvider`, resources to `MemoryBackend` or read-only stores.
- **Policy enforcement**: Validate server permission scopes (network/filesystem) before allowing tool registration.
- **Safety wrappers**: Add rate limits, timeouts, caching, and auditing to each exposed tool.
- **Sampling support**: Handle MCP `sampling/createMessage` by delegating to parent agent (bounded depth and token budget) or denying per policy.

### Code sketch (Rust)

```rust
// src/extensions/mcp_bridge.rs

pub struct McpBridgeExtension {
    config: McpConfig,
    servers: RwLock<HashMap<String, McpServerHandle>>,
    client: McpClient,
    policy: PolicyEngine,
}

impl Extension for McpBridgeExtension {
    fn metadata(&self) -> &ExtensionMetadata { /* ... */ }

    fn capabilities(&self) -> &[Capability] {
        // This extension registers tools and manages resources
        &[Capability::ToolProvider("mcp".into()), Capability::EventHandler("tool_call".into())]
    }

    fn initialize(&self, ctx: &ExtensionContext) -> Result<()> {
        for server_cfg in &self.config.servers {
            if let Err(e) = self.start_and_register(server_cfg, ctx) {
                error!(%server_cfg.name, ?e, "Failed to start MCP server");
            }
        }
        Ok(())
    }
}

impl McpBridgeExtension {
    fn start_and_register(&self, cfg: &McpServerConfig, ctx: &ExtensionContext) -> Result<()> {
        // 1. Launch server (stdio) or connect (SSE)
        let server = self.client.spawn_stdio(&cfg.command, &cfg.args, &cfg.env)?;

        // 2. Negotiate capabilities
        let caps = server.initialize()?;
        let tools = server.list_tools()?;
        let resources = server.list_resources()?;

        // 3. Policy check
        self.policy.validate_server(cfg, &tools, &resources)?;

        // 4. Register each tool with wrapped, monitored impl
        for tool in tools {
            let wrapped = McpToolAdapter::new(server.clone(), tool)
                .with_timeout(Duration::from_secs(cfg.timeout_secs.unwrap_or(30)))
                .with_retries(3)
                .with_audit_log(ctx.audit_sender.clone());

            ctx.register_tool(wrapped.name(), Arc::new(wrapped));
        }

        // 5. Register resource adapters (e.g. read-only memory backend)
        for res in resources {
            let adapter = McpResourceAdapter::new(server.clone(), res);
            ctx.register_memory_backend(res.uri_prefix(), Arc::new(adapter));
        }

        // 6. If sampling supported, register controlled sampler
        if caps.sampling {
            let sampler = ControlledSampler::new(server, ctx.clone());
            ctx.register_tool("mcp.sample", Arc::new(sampler));
        }

        self.servers.write().insert(cfg.name.clone(), server);
        Ok(())
    }
}

// Tool adapter adds policy, limits, observability
struct McpToolAdapter {
    server: McpServerHandle,
    inner: ToolInfo,
    limits: CallLimits,
}

impl Tool for McpToolAdapter {
    fn call(&self, args: Value) -> Result<Value> {
        self.limits.check()?;
        let start = Instant::now();
        let result = self.server.call_tool(&self.inner.name, args);
        record_latency(start.elapsed());
        result
    }
}
```

---

## 4. Server Hosting Extension (optional)

A complementary extension can act as an **MCP server host** — managing pools of trusted servers, sandboxing their execution (resource limits, seccomp), and auto-discovering servers from manifests.

- Declarative server packages in `~/.vulcan/extensions/mcp-servers/` with `server.toml` (command, allowed capabilities, resource limits).
- Supervisor restarts crashed servers, enforces memory/time budgets, and exposes server status/health as first-class resources.
- Useful for shipping curated, audited servers (e.g., `mcp-server-postgres`, `mcp-server-slack`) as signed `.vpk` packages alongside normal extensions.

---

## Promotion Benefits

| Stage | Benefit |
|-------|---------|
| **Skill** | Human-readable instructions; fast to author |
| **Draft Extension** | Structured config, schema validation, dependency checks, UI preference toggle |
| **Code Extension** | Programmatic lifecycle, policy enforcement, robust error handling, performance (caching/retries), secure sandboxing |

Promoting MCP to an extension lets teams treat MCP servers as **managed infrastructure** rather than ad-hoc scripts — with observability, policy, and reliability appropriate for production agents.

---

## Quick comparison

| Aspect | Ad-hoc MCP (skill) | MCP Bridge (code extension) |
|--------|---------------------|-----------------------------|
| Configuration | Manual edits to config | Validated schema + UI |
| Tool registration | Manual or one-shot | Automatic, dynamic, monitored |
| Policy | Human discipline | Enforced via capability scopes |
| Retry/circuit-breaker | None | Built into adapter |
| Observability | Basic logs | Metrics, spans, audit trail |
| Sampling support | Manual handling | Controlled delegation with depth limits |
