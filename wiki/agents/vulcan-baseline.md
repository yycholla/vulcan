# Vulcan Baseline

## Product Shape

Vulcan is a Rust, terminal-first personal AI agent. The current product goal is a fast, inspectable TUI/CLI/daemon workflow for coding and operational work, not a broad consumer chatbot or SaaS dashboard.

Current product language:

- Frontends talk to a long-lived daemon.
- The daemon owns expensive runtime resources.
- Sessions own conversation state, provider selection, hook instances, tool registry filtering, cancellation, and active turns.
- Subagents are child sessions, not ad hoc in-process child agents.

Primary local evidence:

- `README.md`
- `PRODUCT.md`
- `CONTEXT.md`
- `docs/adr/0001-daemon-required-frontends.md`
- `docs/adr/0002-shared-runtime-resource-pool.md`
- `src/runtime_pool.rs`
- `src/daemon/session.rs`
- `src/daemon/session_agent.rs`

## Runtime Architecture

Vulcan's current architecture is more advanced than the older direct-agent overview suggests:

- `RuntimeResourcePool` owns shared session/run/artifact stores, orchestration store, LSP manager, optional Cortex memory, extension registry, extension audit log, and extension state store.
- `SessionState` lazily installs a warm `Agent` per session and dedupes concurrent first touches.
- Gateway lanes map to stable daemon session IDs through `DaemonLaneRouter`.
- The turn runner emits domain-level `TurnEvent`s that are adapted into stream frames.

This is already close to the right shape for multi-frontend operation.

## Hooks And Extensions

Vulcan has a broad hook surface. The docs still summarize an older five-event view, but `src/hooks/mod.rs` includes more lifecycle points:

- raw input interception
- prompt injection
- turn start/end
- message start/update/end
- tool execution start/update/end
- before/after provider request
- before/after tool call
- context rewrite
- compaction control
- session fork/shutdown

The extension design is strong:

- Cargo-crate extension distribution through `inventory`.
- Daemon-side and frontend-side extension split.
- Session-local extension instantiation.
- `ToolResult.details` as a replayable structured payload.
- Extension state store and frontend events/status widgets.

Open risk: multiple extension runtimes are still conceptually in play. The safest policy is a trust ladder:

- trusted first-party code: native Cargo extensions
- third-party code: WASM/Wasmtime later
- external integrations: MCP/subprocess

## Tools, Results, And Replay

The current code already has structured `ToolResult`:

- `output` for the LLM
- `media` for attachments
- `is_error` for structured failure
- `details` for frontend renderers and replay
- `display_preview` for TUI cards
- `edit_diff` for per-call diagnostics

So "add ToolResult" is not a current gap. The better gap is using this structure more aggressively for artifacts, replay, and frontend-specific rendering.

## Memory And Recall

Vulcan has multiple memory surfaces:

- SQLite session storage with history.
- Recall hook over session history.
- Optional Cortex graph memory with vector search, edges, decay, and daemon-owned storage.
- Run records and artifacts.

What is less clear than Hermes: a small, user-visible, bounded "what the agent knows about me and this workspace" memory contract. Cortex is powerful, but the product needs inspectable controls before autonomous memory/skill mutation gets broad authority.

## MCP

Vulcan already has an MCP bridge:

- stdio-only first slice
- opt-in server config
- default disabled exposure
- namespaced tool adapter
- sampling policy and supervisor modules

Current gap is not "build MCP"; it is catalog/config UX, safety posture, and per-server/tool review.

## Subagents

Vulcan already has bounded child agents through `spawn_subagent`:

- daemon child sessions
- conservative read-only default allowlist
- hard iteration cap
- parent cancellation propagation
- orchestration store integration

Deferred by code comments:

- token budget tracking
- transcript/artifact handle for inspection
- richer TUI subagent timeline

Those are better next steps than adding a second delegation system.

## Gateway And Scheduling

Vulcan has a gateway foundation:

- Axum gateway routes
- durable inbound/outbound queue concepts
- Discord/Telegram/loopback connectors
- lane-to-daemon-session mapping
- scheduler config and gateway scheduler modules

Compared with Hermes, Vulcan is much narrower in connector breadth. That is fine. The gap is hardening the common gateway contract and choosing one or two high-leverage connector additions, not racing to 20 platforms.

## Local Staleness Found

- `CLAUDE.md` points to `~/wiki/queries/rust-hermes-plan.md`, but that path did not exist in this environment.
- GitNexus docs in `CLAUDE.md` say Vulcan is indexed, but `npx gitnexus status` reported "Repository not indexed."
- `src/tools/CONTEXT.md` says `Tool::call` returns `Result<String>`, but current code returns `Result<ToolResult>`.
