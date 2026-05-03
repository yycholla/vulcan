<!-- refreshed: 2026-05-03 -->
# Architecture

**Analysis Date:** 2026-05-03

## System Overview

```text
+-------------------------------------------------------------+
|                       Frontends                              |
+-------------------+------------------+----------------------+
| CLI / one-shot    | TUI              | Gateway HTTP         |
| `vulcan/src/main.rs` | `src/tui/mod.rs` | `src/gateway/mod.rs` |
+---------+---------+---------+--------+----------+-----------+
          |                   |                   |
          v                   v                   v
+-------------------------------------------------------------+
|                    Daemon / Session Runtime                  |
| `src/daemon/`, `src/client/`, `src/runtime_pool.rs`          |
+-------------------------------------------------------------+
          |
          v
+-------------------------------------------------------------+
|                 Long-Lived Agent Per Session                 |
| `src/agent/mod.rs`, `src/agent/run.rs`, `src/agent/turn.rs`  |
+---------+-------------------+-------------------+-----------+
          |                   |                   |
          v                   v                   v
+----------------+  +----------------+  +---------------------+
| Hooks          |  | Tools          |  | Providers           |
| `src/hooks/`   |  | `src/tools/`   |  | `src/provider/`     |
+----------------+  +----------------+  +---------------------+
          |                   |                   |
          v                   v                   v
+-------------------------------------------------------------+
| Storage, Extensions, Code Intelligence, Platforms            |
| `src/memory/`, `src/extensions/`, `src/code/`, `src/platform/`|
+-------------------------------------------------------------+
```

## Component Responsibilities

| Component | Responsibility | File |
|-----------|----------------|------|
| Binary entry | Parse CLI, load config, route commands, link first-party extension crates | `vulcan/src/main.rs` |
| CLI command model | Define global flags and user-facing subcommands | `src/cli.rs` |
| Agent | Own one session's provider, tools, hooks, memory, context, run records, artifacts, trust profile, and turn cancellation | `src/agent/mod.rs` |
| Turn runner | Unify buffered and streaming turn execution behind domain `TurnEvent`s | `src/agent/turn.rs`, `src/agent/run.rs` |
| Hook registry | Provide ordered hook events for input, context, provider, tool, compaction, and session lifecycle interception | `src/hooks/mod.rs` |
| Tool registry | Register built-in and extension tools, filter by workspace/profile, and expose OpenAI-compatible tool schemas | `src/tools/mod.rs` |
| Provider layer | Define OpenAI-compatible messages, tool calls, streaming events, provider taxonomy, and `LLMProvider` | `src/provider/mod.rs`, `src/provider/openai.rs` |
| Session store | Persist sessions/messages in SQLite with FTS5 and gateway queues in the same schema family | `src/memory/mod.rs`, `src/memory/schema.rs` |
| Daemon server | Own Unix-socket accept loop, request dispatch, sessions, and process lifecycle | `src/daemon/server.rs`, `src/daemon/dispatch.rs`, `src/daemon/session.rs` |
| Runtime pool | Share expensive daemon resources across sessions: stores, LSP manager, cortex store, extension registry, audit log | `src/runtime_pool.rs` |
| TUI | Run terminal event loop, own UI state, render streaming events, route pause prompts and frontend extensions | `src/tui/mod.rs`, `src/tui/state/mod.rs` |
| Gateway | Expose Axum routes, durable queues, scheduler, lane routing, and platform connector dispatch | `src/gateway/mod.rs`, `src/gateway/server.rs`, `src/gateway/worker.rs` |
| Extensions | Register daemon and session extension capabilities through inventory and per-session runtime wiring | `src/extensions/api.rs`, `src/extensions/registry.rs` |
| Symphony | Run workflow/task-source orchestration on top of Vulcan primitives | `src/symphony/` |

## Pattern Overview

**Overall:** Local-first daemon-backed agent runtime with session-scoped composition.

**Key Characteristics:**
- Keep `Agent` long-lived per session; do not construct a fresh agent for each prompt.
- Route buffered, streaming, TUI, daemon, and gateway turns through the same turn runner vocabulary.
- Treat hooks, tools, provider selection, session history, and frontend capabilities as session-local state.
- Put expensive adapters and durable stores behind daemon-owned shared pools.
- Gate optional surfaces with Cargo features: `daemon`, `gateway`, and `telegram` in `Cargo.toml`.

## Layers

**Frontend Layer:**
- Purpose: Accept user/platform input and render output.
- Location: `vulcan/src/main.rs`, `src/cli_*.rs`, `src/tui/`, `src/gateway/routes/`
- Contains: Clap command routing, terminal UI, HTTP route handlers, connector entrypoints.
- Depends on: `src/config/`, `src/client/`, `src/agent/`, `src/gateway/`, `src/tui/`
- Used by: Users, tests, gateway platform connectors.

**Daemon And Session Layer:**
- Purpose: Own process lifecycle, session map, request routing, and shared resources.
- Location: `src/daemon/`, `src/client/`, `src/runtime_pool.rs`
- Contains: Unix-socket protocol, request dispatcher, per-session `AgentHandle`, lazy agent assembly, daemon autostart client.
- Depends on: `src/agent/`, `src/extensions/`, `src/memory/`, `src/run_record/`, `src/artifact/`
- Used by: CLI/TUI daemon flows and gateway lane dispatch.

**Agent Runtime Layer:**
- Purpose: Execute one session's turns and preserve conversation-specific state.
- Location: `src/agent/`
- Contains: `Agent`, `AgentBuilder`, `TurnRunnerMut`, turn preparation, compaction, provider calls, tool dispatch, session resume.
- Depends on: `src/provider/`, `src/tools/`, `src/hooks/`, `src/memory/`, `src/context.rs`
- Used by: Direct CLI fallback, TUI, daemon prompt handlers, gateway session turns.

**Extension And Hook Layer:**
- Purpose: Intercept and extend session behavior without duplicating the agent loop.
- Location: `src/hooks/`, `src/extensions/`, `vulcan-core-ext-*`, `vulcan-ext-*`
- Contains: `HookRegistry`, built-in hooks, daemon/session extension traits, manifests, lifecycle policy.
- Depends on: `src/provider/`, `src/tools/`, `src/pause.rs`, `vulcan-frontend-api/`
- Used by: Agent construction, daemon runtime pool, TUI frontend surfaces.

**Tool And Code Intelligence Layer:**
- Purpose: Expose local capabilities to models through structured tool schemas.
- Location: `src/tools/`, `src/code/`
- Contains: File/shell/git/web/cargo/code/LSP/semantic tools, sandbox checks, profile filtering, parser caches.
- Depends on: `src/provider::ToolDefinition`, tree-sitter, LSP manager, filesystem and git adapters.
- Used by: Agent turn dispatch and extension-contributed tools.

**Provider Layer:**
- Purpose: Normalize model/provider interaction into OpenAI-compatible chat and streaming APIs.
- Location: `src/provider/`
- Contains: `LLMProvider`, `Message`, `ToolCall`, `ToolDefinition`, `StreamEvent`, OpenAI-compatible HTTP client, catalog/factory, redaction.
- Depends on: `reqwest`, `serde`, configured provider profiles.
- Used by: Agent turn collection and provider/model CLI commands.

**Persistence Layer:**
- Purpose: Store sessions, run records, artifacts, gateway queues, scheduler runs, cortex memory, and local knowledge indexes.
- Location: `src/memory/`, `src/run_record/`, `src/artifact/`, `src/knowledge/`, `src/context_pack/`
- Contains: SQLite schema/codec/store adapters, FTS5 search, queue tables, in-memory fallbacks for tests.
- Depends on: `rusqlite`, `r2d2_sqlite` under gateway, `cortex-memory-core` when enabled.
- Used by: Agent, daemon, gateway, CLI inspection commands.

## Data Flow

### Primary Request Path

1. CLI parses command and loads config (`vulcan/src/main.rs:27`, `vulcan/src/main.rs:73`).
2. TUI or prompt path constructs/reuses an `Agent`; TUI wraps it in `Arc<tokio::sync::Mutex<Agent>>` (`src/tui/mod.rs:166`, `src/tui/mod.rs:225`).
3. `AgentBuilder` resolves provider, tools, hooks, memory, extensions, and trust state (`src/agent/mod.rs:279`, `src/agent/mod.rs:302`).
4. A turn starts a run record and delegates to `TurnRunnerMut` (`src/agent/run.rs:236`, `src/agent/run.rs:261`).
5. `prepare_turn_impl` builds the system prompt, tool definitions, history, and immediate user-message persistence (`src/agent/run.rs:398`).
6. Hooks can rewrite transient context, block/rewrite tool calls, observe provider messages, or force/validate compaction (`src/hooks/mod.rs`).
7. Provider calls run through `LLMProvider::chat` or `LLMProvider::chat_stream`, producing `ChatResponse` or `StreamEvent`s (`src/provider/mod.rs`).
8. Tool calls dispatch through `ToolRegistry` and return `ToolResult` to the model (`src/tools/mod.rs`).
9. Final response, run status, and session history are persisted through agent stores (`src/agent/run.rs:216`, `src/memory/mod.rs`).

### TUI Streaming Flow

1. `run_tui` initializes terminal input, stream channel, audit hook, pause channel, frontend capabilities, and long-lived agent (`src/tui/mod.rs:166`).
2. A prompt task calls `Agent::run_prompt_stream_with_cancel`, which opens a TUI-origin run record (`src/agent/run.rs:313`).
3. Turn runner emits `TurnEvent`s; the stream body forwards non-terminal events as `StreamEvent`s for rendering (`src/agent/run.rs:338`).
4. TUI state consumes stream/tool/pause/frontend events and redraws via `render_view` (`src/tui/mod.rs`).

### Daemon RPC Flow

1. `Server::run` accepts Unix-socket connections and spawns a task per connection (`src/daemon/server.rs`).
2. `Dispatcher::dispatch` routes method names to handler modules and returns either a single `Response` or streaming frames (`src/daemon/dispatch.rs:35`, `src/daemon/dispatch.rs:41`).
3. `prompt.run` and `prompt.stream` resolve the session, ensure/lazy-build an agent, apply input hooks, and execute the turn (`src/daemon/handlers/prompt.rs`).
4. Responses and stream frames are serialized back through one writer queue per connection (`src/daemon/server.rs`).

### Gateway Platform Flow

1. `gateway::run` validates `[gateway]`, binds Axum, opens gateway SQLite pool, and registers platforms (`src/gateway/mod.rs:40`, `src/gateway/mod.rs:69`).
2. `/v1/inbound` or webhook routes enqueue `InboundMessage`s into `InboundQueue` (`src/gateway/server.rs`, `src/gateway/queue.rs`).
3. `spawn_inbound_dispatcher` claims rows, serializes work per lane, and calls `worker::process_one` (`src/gateway/mod.rs:265`).
4. `DaemonLaneRouter` maps platform/chat lanes to stable daemon session ids (`src/gateway/lane_router.rs`).
5. `GatewayDaemonClient` sends prompts to daemon sessions; outbound rows are rendered and dispatched through `PlatformRegistry` (`src/gateway/daemon_client.rs`, `src/gateway/outbound.rs`, `src/gateway/registry.rs`).

**State Management:**
- Live session transcript is `Agent.history_cache`; `SessionStore` is the durability/recovery adapter (`src/agent/mod.rs:130`, `src/memory/mod.rs`).
- Daemon sessions live in `SessionMap` and hold optional warm `AgentHandle`s (`src/daemon/session.rs`).
- TUI state lives in `src/tui/state/mod.rs` and mirrors selected agent surfaces, not the whole agent.
- Gateway queues are durable SQLite rows; lane serialization is in-memory process state plus stable daemon session ids.

## Key Abstractions

**Agent:**
- Purpose: Session-scoped aggregate for provider, tools, hooks, memory, run records, artifacts, trust, and cancellation.
- Examples: `src/agent/mod.rs`, `src/agent/run.rs`, `src/agent/session.rs`
- Pattern: Builder-based construction with optional daemon resource pool.

**TurnRunner / TurnEvent:**
- Purpose: Single vocabulary for buffered, streaming, TUI, gateway, and daemon turn execution.
- Examples: `src/agent/turn.rs`, `src/agent/run.rs`
- Pattern: Adapter seam around domain events; frontends convert to their wire/UI types.

**HookRegistry / HookHandler:**
- Purpose: Ordered session-local event bus for context injection, provider observation, tool gating, compaction policy, input interception, and lifecycle events.
- Examples: `src/hooks/mod.rs`, `src/hooks/approval.rs`, `src/hooks/safety.rs`, `src/hooks/skills.rs`
- Pattern: Trait methods default to no-op; first non-continue outcome wins for blocking events.

**Tool / ToolRegistry / ToolResult:**
- Purpose: Model-callable capabilities with structured result, media, details, display preview, and edit diff metadata.
- Examples: `src/tools/mod.rs`, `src/tools/file.rs`, `src/tools/git.rs`, `src/tools/lsp.rs`
- Pattern: `async_trait` tools registered into a context-filtered registry.

**LLMProvider:**
- Purpose: Provider-agnostic chat and streaming contract.
- Examples: `src/provider/mod.rs`, `src/provider/openai.rs`, `src/provider/factory.rs`
- Pattern: OpenAI-compatible transport with local typed `ProviderError`.

**RuntimeResourcePool:**
- Purpose: Daemon-owned shared handles for SQLite stores, orchestration, LSP, cortex, extensions, and audit log.
- Examples: `src/runtime_pool.rs`, `src/daemon/session_agent.rs`
- Pattern: `Arc`-cloned adapters handed to per-session agents.

**Daemon Protocol:**
- Purpose: Length-delimited JSON RPC over Unix sockets with normal responses, streaming frames, and push frames.
- Examples: `src/daemon/protocol.rs`, `src/client/transport.rs`, `src/daemon/server.rs`
- Pattern: Request ids demultiplex concurrent calls on one client connection.

**Platform / PlatformRegistry:**
- Purpose: Gateway connector abstraction for inbound/outbound platform messages and capability-aware rendering.
- Examples: `src/platform/mod.rs`, `src/gateway/registry.rs`, `src/gateway/discord.rs`, `src/gateway/telegram.rs`
- Pattern: Named routing registry over `Arc<dyn Platform>`.

## Entry Points

**Binary:**
- Location: `vulcan/src/main.rs`
- Triggers: `cargo run`, installed `vulcan` binary.
- Responsibilities: Link first-party extension crates, parse CLI, initialize observability, load/repair config, route to TUI/CLI/daemon/gateway commands.

**Library Root:**
- Location: `src/lib.rs`
- Triggers: Workspace crates, tests, binary crate.
- Responsibilities: Export core modules and feature-gated daemon/gateway modules.

**TUI:**
- Location: `src/tui/mod.rs`
- Triggers: `vulcan`, `vulcan chat`, `vulcan session <id>`.
- Responsibilities: Terminal initialization, long-lived agent construction, render loop, input loop, streaming event pump, pause/frontend surfaces.

**Daemon:**
- Location: `src/daemon/cli.rs`, `src/daemon/server.rs`
- Triggers: `vulcan daemon start`, daemon autostart from `src/client/auto_start.rs`.
- Responsibilities: Unix-socket lifecycle, session map, dispatcher, shared resource pool, config reload and status.

**Gateway:**
- Location: `src/gateway/mod.rs`, `src/cli_gateway.rs`
- Triggers: `vulcan gateway run --bind <addr>` with `--features gateway`.
- Responsibilities: Axum server, gateway config validation, queue recovery, connector spawn, scheduler, inbound/outbound workers.

**Extension Crates:**
- Location: `vulcan-ext-*`, `vulcan-core-ext-*`, `vulcan-extension-macros/`
- Triggers: Link-time `inventory::submit!` registration from the binary crate.
- Responsibilities: Add first-party hooks, tools, frontend descriptors, and demo extensions.

## Architectural Constraints

- **Threading:** Tokio async runtime; TUI uses a dedicated blocking input thread plus async channels; daemon spawns per-connection and per-request tasks; gateway uses Axum plus background worker tasks.
- **Global state:** `inventory` link-time registration is process-global for extensions; daemon process owns shared resources through `RuntimeResourcePool`; test-only globals should stay isolated.
- **Circular imports:** Not detected in Rust module imports during this mapping; the main coupling risk is large domain modules depending on shared root modules through `crate::*`.
- **Feature gates:** `src/daemon/` is behind `daemon` in `src/lib.rs`; `src/gateway/` and gateway CLI are behind `gateway`; `src/gateway/telegram.rs` is behind `telegram`.
- **Session lifetime:** Stateful hooks, approval caches, audit buffers, cancellation, and history cache require one `Agent` per live session.
- **History invariants:** Tool messages must have matching assistant tool-call ids; compaction rewrites are validated before replacing history.

## Anti-Patterns

### Per-Prompt Agent Construction

**What happens:** New code calls `Agent::builder(config).build()` for each prompt instead of reusing a session agent.
**Why it's wrong:** Hook state, approval caches, session history cache, provider selection, run lineage, and cancellation state are lost.
**Do this instead:** Reuse the TUI/daemon `Arc<Mutex<Agent>>` or session `AgentHandle` patterns in `src/tui/mod.rs` and `src/daemon/session.rs`.

### Parallel Buffered And Streaming Logic

**What happens:** New behavior is added separately to `run_prompt` and `run_prompt_stream` paths.
**Why it's wrong:** CLI, TUI, daemon, and gateway diverge on hooks, compaction, tool dispatch, and persistence.
**Do this instead:** Put turn behavior behind `TurnRunnerMut` and adapt `TurnEvent` to the caller-specific surface in `src/agent/turn.rs` and `src/agent/run.rs`.

### Bypassing Hooks Around Tools Or Input

**What happens:** A tool or prompt path invokes work directly without `apply_on_input`, `before_tool_call`, or `after_tool_call`.
**Why it's wrong:** Safety, approval, audit, native-tool enforcement, diagnostics, extension interception, and compaction policy stop applying.
**Do this instead:** Route user input through agent/daemon prompt handlers and route tool execution through agent dispatch in `src/agent/dispatch.rs`.

### Gateway-Owned Agent State

**What happens:** Gateway code caches or builds agents per platform lane.
**Why it's wrong:** The daemon owns session lifecycle and resource pooling; gateway lane state should be routing state only.
**Do this instead:** Map lanes to daemon session ids with `DaemonLaneRouter` and use `GatewayDaemonClient` from `src/gateway/lane_router.rs` and `src/gateway/daemon_client.rs`.

## Error Handling

**Strategy:** Use `anyhow::Result` at orchestration boundaries, local typed errors where callers need behavior-specific handling, and daemon/gateway protocol errors for wire-visible failures.

**Patterns:**
- Provider failures use `ProviderError` taxonomy in `src/provider/mod.rs`.
- Daemon RPC failures use `ProtocolError` and `Response::error` in `src/daemon/protocol.rs`.
- Gateway queue and lane errors are logged and retried/dead-lettered through `src/gateway/queue.rs`.
- Hook handler errors/timeouts are isolated and counted by `HookRegistry` instead of breaking the agent loop.

## Cross-Cutting Concerns

**Logging:** `tracing` spans are initialized in `vulcan/src/main.rs`; daemon/gateway request spans live in `src/observability.rs`.
**Validation:** Config validates at load or before socket bind; gateway validates `[gateway]` before binding; extension manifests/policies validate in `src/extensions/`.
**Authentication:** Provider API keys come from config/env; local providers can run keyless; gateway `/v1/*` uses constant-time bearer checks in `src/gateway/server.rs`; webhooks use platform-specific verification.

---

*Architecture analysis: 2026-05-03*
