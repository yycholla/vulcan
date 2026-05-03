---
title: ARCHITECTURE
last_mapped_commit: f5e07ca31dd7a4ac794b9838884bfdad74a7e37c
mapped_at: 2026-05-03
scope: full repo
---

# Architecture

Vulcan is organized around a long-lived agent runtime, a daemon-owned resource pool, hook-mediated tool/provider execution, and multiple frontends that share the same backend behavior.

## Entry Points

- Binary entry point: `vulcan/src/main.rs`.
- Library root: `src/lib.rs`.
- CLI shape is defined in `src/cli.rs` and split into command modules like `src/cli_provider.rs`, `src/cli_gateway.rs`, `src/cli_extension.rs`, and `src/cli_run.rs`.
- TUI entry and state wiring live in `src/tui/mod.rs`, `src/tui/init.rs`, and `src/tui/ui_runtime.rs`.
- Gateway daemon mode enters through CLI handling and modules under `src/gateway/`.

## Runtime Model

- The architectural target is daemon-required frontends, documented in `docs/adr/0001-daemon-required-frontends.md`.
- Shared runtime resources are documented in `docs/adr/0002-shared-runtime-resource-pool.md`.
- Runtime pooling code lives in `src/runtime_pool.rs`.
- Daemon lifecycle and session ownership live in `src/daemon/lifecycle.rs`, `src/daemon/session.rs`, and `src/daemon/state.rs`.
- Client/daemon communication lives in `src/client/` and `src/daemon/protocol.rs`.

## Agent Loop

- Core agent code lives in `src/agent/`.
- Turn orchestration is split across `src/agent/turn.rs`, `src/agent/run.rs`, and `src/agent/dispatch.rs`.
- Session state is modeled in `src/agent/session.rs`.
- Provider invocation is wrapped by `src/agent/provider.rs`.
- Agent tests live in `src/agent/tests.rs` and integration coverage in `tests/agent_loop.rs`.

## Hook System

- Hook contracts and registry logic live in `src/hooks/mod.rs`.
- Built-in hooks include audit, approval, cortex capture/recall, diagnostics, native-tool preference, recall, RTK, safety, and skills under `src/hooks/`.
- The hook flow is a foundation surface: `BeforePrompt`, `BeforeToolCall`, `AfterToolCall`, `BeforeAgentEnd`, plus session lifecycle events.
- Tool execution runs through hook guards before and after calls.
- Skills are injected through `src/hooks/skills.rs`, not by hard-coding them in the prompt builder.

## Provider Flow

- Provider interfaces live in `src/provider/mod.rs`.
- OpenAI-compatible HTTP behavior lives in `src/provider/openai.rs`.
- Catalog and factory behavior lives in `src/provider/catalog.rs` and `src/provider/factory.rs`.
- Sanitization and redaction live in `src/provider/think_sanitizer.rs` and `src/provider/redact.rs`.
- Both streaming and non-streaming provider paths must preserve hook event semantics.

## Tools

- Tool registry and dispatch live in `src/tools/mod.rs`.
- Individual tool implementations live under `src/tools/`, including file, shell, PTY, web, Git, code, LSP, and profile tools.
- Filesystem safety is centralized in `src/tools/fs_sandbox.rs`.
- Tool behavior is surfaced to the TUI through tool cards and rendering modules under `src/tui/`.

## Frontend And TUI

- TUI architecture is documented in `src/tui/CONTEXT.md`.
- UI state and rendering are split across `src/tui/state/` style modules, `src/tui/rendering.rs`, `src/tui/views.rs`, and `src/tui/surface.rs`.
- Slash commands, keybindings, model picker, provider picker, prompt input, and orchestration UI live in `src/tui/`.
- Frontend extension capability contracts use `vulcan-frontend-api/` and `src/extensions/`.

## Gateway

- Gateway architecture is documented in `src/gateway/CONTEXT.md`.
- Lane routing is in `src/gateway/lane.rs` and `src/gateway/lane_router.rs`.
- Workers and schedulers are in `src/gateway/worker.rs`, `src/gateway/scheduler.rs`, and `src/gateway/scheduler_store.rs`.
- Gateway routes, queue persistence, and platform connectors live in `src/gateway/server.rs`, `src/gateway/queue.rs`, `src/gateway/discord.rs`, and `src/gateway/telegram.rs`.
- Gateway lanes map inbound platform messages to long-lived daemon sessions.

## Extensions

- Extension architecture is documented in `src/extensions/CONTEXT.md` and ADRs `docs/adr/0003-extension-daemon-frontend-split.md` through `docs/adr/0007-extension-frontend-events-and-status-widgets.md`.
- Daemon extension behavior and frontend capabilities are intentionally split.
- Extension manifests and policy checks live in `src/extensions/manifest.rs`, `src/extensions/policy.rs`, and `src/extensions/verify.rs`.
- First-party extension crates are linked by the binary crate.

## Symphony

- Symphony code lives in `src/symphony/`.
- Workflow contract and typed config are documented in `docs/adr/0007-symphony-workflow-contract.md` and `docs/adr/0008-symphony-typed-config.md`.
- Symphony reads workflow files, normalizes tasks, prepares workspaces, and launches agent workers.
- Symphony is daemon-adjacent and should consume base Vulcan capabilities rather than owning duplicate infrastructure.

## Observability

- Observability setup is centralized in `src/observability.rs`.
- Instrumented surfaces are configured in `config.example.toml`.
- The intended model is full-surface tracing and metrics when telemetry is enabled.
- TUI logging is kept away from the terminal surface, while one-shot CLI paths log to stderr.
