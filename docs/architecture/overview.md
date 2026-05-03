<!-- generated-by: gsd-doc-writer -->
# Architecture

Vulcan is organized as a Rust workspace around a shared `vulcan` library crate. The root CLI parses user intent in `src/cli.rs`, then routes execution into the agent loop, TUI, daemon, gateway, configuration, and extension subsystems. The architecture is local-first: persistent state lives under the Vulcan home directory, provider traffic goes through OpenAI-compatible clients, and optional gateway connectors reuse the same daemon-backed agent runtime.

## Component Diagram

```text
CLI/TUI frontends
  -> Config loader
  -> Agent builder
  -> Hook registry
  -> Provider client
  -> Tool registry
  -> Session and run storage

Gateway routes
  -> Inbound queue
  -> Daemon lane router
  -> Daemon client
  -> Outbound queue
  -> Platform registry

Extension crates
  -> Extension manifests
  -> Inventory registration
  -> Runtime extension registry
```

## Data Flow

For direct chat and prompt flows, Clap parses the command tree from `src/cli.rs`, configuration is loaded from the Vulcan config directory, and an `Agent` is built around provider, hooks, tools, and session state. A user message enters the agent loop, hooks can inject context or gate tool calls, tools return string results, and the provider client produces the next assistant response.

For gateway flows, `build_router(` in `src/gateway/server.rs` exposes public health, bearer-authenticated `/v1/*` routes, and platform-specific webhooks. Inbound messages are written to `InboundQueue`, lane routing maps a platform chat to a daemon session id, the daemon client sends the prompt to the long-lived backend, and outbound messages are delivered through `PlatformRegistry`.

## Key Abstractions

| Abstraction | Location | Role |
|-------------|----------|------|
| `Cli` | `src/cli.rs` | Top-level command-line parser and global flags. |
| `Command` | `src/cli.rs` | User-visible command enum for chat, prompt, sessions, gateway, daemon, provider, config, and diagnostics. |
| `Agent` | `src/agent/mod.rs` | Core runtime that coordinates messages, provider calls, hooks, and tool execution. |
| `HookRegistry` | `src/hooks/mod.rs` | Event pipeline for prompt injection, tool-call gating, and end-of-turn behavior. |
| `Config` | `src/config/mod.rs` | User configuration loaded from Vulcan config files and defaults. |
| `AppState` | `src/gateway/server.rs` | Shared Axum state for gateway routes and queues. |
| `DaemonLaneRouter` | `src/gateway/lane_router.rs` | Stable mapping from gateway lanes to daemon session ids. |
| `ExtensionRegistry` | `src/extensions/registry.rs` | Runtime registry for extension metadata, lifecycle, and contributed capabilities. |

## Directory Structure Rationale

```text
src/agent/       core agent loop and session-facing abstractions
src/client/      daemon client protocol support
src/config/      config structs, loading, migration, and tests
src/daemon/      long-lived backend process and RPC dispatch
src/extensions/  extension metadata, manifests, registry, and state
src/gateway/     optional HTTP gateway, queues, scheduler, and platforms
src/hooks/       hook handlers and hook registry
src/provider/    provider factory and OpenAI-compatible implementation
src/tools/       built-in tool implementations and policy profiles
src/tui/         terminal UI state, rendering, input, and widgets
```

Workspace member crates separate binaries, frontend-extension API contracts, procedural macros, and example/core extensions from the root runtime crate while sharing one lockfile and one test surface.
