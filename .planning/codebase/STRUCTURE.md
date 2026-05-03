# Codebase Structure

**Analysis Date:** 2026-05-03

## Directory Layout

```text
vulcan/
├── Cargo.toml                  # Workspace manifest and root `vulcan-core` library crate
├── vulcan/                     # Thin binary crate; `vulcan/src/main.rs`
├── src/                        # Root library crate implementation
├── vulcan-frontend-api/        # Shared frontend extension API crate
├── vulcan-tui/                 # Companion TUI support crate
├── vulcan-extension-macros/    # Procedural macros for extension manifests/registration
├── vulcan-core-ext-*/          # Core first-party extension crates
├── vulcan-ext-*/               # First-party feature/demo extension crates
├── tests/                      # Root integration tests
├── benches/                    # Divan and soak benchmark harnesses
├── docs/                       # ADRs, guides, feature docs, references, plans
├── skills/                     # Project/user-facing skills bundled with repo
├── .agents/skills/             # Project-local agent skill indexes
├── .planning/codebase/         # GSD codebase map output
└── config.example.toml         # Configuration reference
```

## Directory Purposes

**Root `src/`:**
- Purpose: Main `vulcan` library crate exported by the root package.
- Contains: Agent runtime, CLI modules, daemon/client, providers, tools, hooks, TUI, gateway, persistence, extensions, code intelligence, workflow helpers.
- Key files: `src/lib.rs`, `src/agent/mod.rs`, `src/cli.rs`, `src/config/mod.rs`

**`vulcan/`:**
- Purpose: Binary crate that links first-party extensions and calls into the root library.
- Contains: `Cargo.toml`, `README.md`, `src/main.rs`.
- Key files: `vulcan/src/main.rs`

**`src/agent/`:**
- Purpose: Session-scoped core runtime.
- Contains: Agent aggregate, builder, turn execution, provider adapters, dispatch, session resume/persistence helpers, tests.
- Key files: `src/agent/mod.rs`, `src/agent/run.rs`, `src/agent/turn.rs`, `src/agent/dispatch.rs`, `src/agent/session.rs`

**`src/daemon/`:**
- Purpose: Long-lived backend process and Unix-socket RPC server.
- Contains: Protocol, server, dispatcher, handlers, session map, state, lifecycle, config watching, eviction, subagent support.
- Key files: `src/daemon/server.rs`, `src/daemon/dispatch.rs`, `src/daemon/session.rs`, `src/daemon/state.rs`, `src/daemon/protocol.rs`

**`src/client/`:**
- Purpose: In-tree daemon client used by frontends and gateway.
- Contains: Transport, errors, autostart, request id demultiplexing, stream handling.
- Key files: `src/client/mod.rs`, `src/client/transport.rs`, `src/client/auto_start.rs`

**`src/provider/`:**
- Purpose: Provider-agnostic LLM contract and OpenAI-compatible implementation.
- Contains: Message/tool schemas, streaming events, provider error taxonomy, catalog, factory, redaction, think sanitizer, mock provider.
- Key files: `src/provider/mod.rs`, `src/provider/openai.rs`, `src/provider/catalog.rs`, `src/provider/factory.rs`

**`src/tools/`:**
- Purpose: Built-in model-callable capabilities.
- Contains: Tool traits, registry, profiles, file/edit/shell/git/web/cargo/code/LSP/spawn tools, filesystem sandboxing, SSRF checks.
- Key files: `src/tools/mod.rs`, `src/tools/file.rs`, `src/tools/shell.rs`, `src/tools/git.rs`, `src/tools/lsp.rs`, `src/tools/fs_sandbox.rs`

**`src/hooks/`:**
- Purpose: Session-local event bus and built-in hook handlers.
- Contains: Hook contracts, registry, approval, audit, safety, diagnostics, skills, recall, cortex, RTK, native-tool preference hooks.
- Key files: `src/hooks/mod.rs`, `src/hooks/approval.rs`, `src/hooks/safety.rs`, `src/hooks/skills.rs`

**`src/tui/`:**
- Purpose: Terminal frontend implementation.
- Contains: `run_tui` orchestrator, state, rendering, event handling, input/keymaps, themes, views, widgets, pickers, frontend extension surfaces.
- Key files: `src/tui/mod.rs`, `src/tui/state/mod.rs`, `src/tui/rendering.rs`, `src/tui/views.rs`, `src/tui/frontend.rs`

**`src/gateway/`:**
- Purpose: Optional HTTP/platform gateway.
- Contains: Axum server, routes, queues, lane routing, daemon client adapter, worker, scheduler, outbound renderer, Discord/Telegram/loopback connectors.
- Key files: `src/gateway/mod.rs`, `src/gateway/server.rs`, `src/gateway/queue.rs`, `src/gateway/lane_router.rs`, `src/gateway/worker.rs`

**`src/extensions/`:**
- Purpose: Cargo-crate extension runtime and policy.
- Contains: Daemon/session extension API, inventory wiring, registry, manifests, install state, audit, policy, verification, draft helpers.
- Key files: `src/extensions/api.rs`, `src/extensions/registry.rs`, `src/extensions/manifest.rs`, `src/extensions/policy.rs`

**`src/memory/`:**
- Purpose: SQLite-backed session and queue persistence plus cortex integration.
- Contains: `SessionStore`, schema migrations, message codec, cortex store, tests.
- Key files: `src/memory/mod.rs`, `src/memory/schema.rs`, `src/memory/codec.rs`, `src/memory/cortex.rs`

**`src/code/`:**
- Purpose: Code intelligence primitives used by tools.
- Contains: Tree-sitter parser cache/graph, embeddings index, LSP manager and requests.
- Key files: `src/code/mod.rs`, `src/code/graph.rs`, `src/code/embed.rs`, `src/code/lsp/mod.rs`

**`src/symphony/`:**
- Purpose: Workflow/task orchestration surface.
- Contains: App server integration, config, task source, workflow parsing, workspace management, runner, orchestrator.
- Key files: `src/symphony/mod.rs`, `src/symphony/workflow.rs`, `src/symphony/orchestrator.rs`

**Workspace Extension Crates:**
- Purpose: First-party extension implementations and supporting APIs.
- Contains: `vulcan-core-ext-skills/`, `vulcan-core-ext-safety/`, `vulcan-ext-auto-commit/`, `vulcan-ext-compact-summary/`, `vulcan-ext-input-demo/`, `vulcan-ext-snake/`, `vulcan-ext-spinner-demo/`, `vulcan-ext-todo/`.
- Key files: `vulcan-core-ext-skills/src/lib.rs`, `vulcan-core-ext-safety/src/lib.rs`, `vulcan-ext-todo/src/lib.rs`

**`docs/`:**
- Purpose: Human and agent reference docs.
- Contains: ADRs, feature docs, guides, architecture/runtime/config/testing overviews, reference reports, local plans, agent workflow docs.
- Key files: `docs/adr/0001-daemon-required-frontends.md`, `docs/adr/0002-shared-runtime-resource-pool.md`, `docs/architecture/overview.md`, `docs/agents/domain.md`

## Key File Locations

**Entry Points:**
- `vulcan/src/main.rs`: Binary startup, config loading, CLI route dispatch, extension crate linking.
- `src/lib.rs`: Public module tree and feature-gated exports.
- `src/cli.rs`: Clap command tree and global flags.
- `src/tui/mod.rs`: TUI frontend entry and event loop.
- `src/daemon/server.rs`: Daemon Unix-socket server loop.
- `src/gateway/mod.rs`: Gateway process entry and background worker wiring.

**Configuration:**
- `Cargo.toml`: Workspace, features, dependencies, bins, benchmarks.
- `config.example.toml`: User-facing Vulcan configuration reference.
- `src/config/mod.rs`: Typed config structs, loading, migration, defaults.
- `src/config_registry.rs`: Config field registry for CLI inspection.
- `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`, `deny.toml`: Toolchain and quality configuration.

**Core Logic:**
- `src/agent/mod.rs`: Agent aggregate and builder.
- `src/agent/run.rs`: Buffered/streaming prompt execution and persistence path.
- `src/agent/turn.rs`: Turn runner seam and `TurnEvent` domain vocabulary.
- `src/hooks/mod.rs`: Hook contracts and registry.
- `src/tools/mod.rs`: Tool contracts and registry.
- `src/provider/mod.rs`: Provider contracts and wire types.
- `src/runtime_pool.rs`: Daemon shared resources.
- `src/memory/mod.rs`: Session persistence.

**Testing:**
- `tests/agent_loop.rs`: Agent loop integration coverage.
- `tests/daemon_e2e.rs`: Daemon integration coverage.
- `tests/client_autostart.rs`: Client/daemon autostart coverage.
- `tests/gateway_no_agent_map.rs`: Gateway architecture regression coverage.
- `src/agent/tests.rs`, `src/config/tests.rs`, `src/memory/tests.rs`: Co-located module tests.
- `vulcan-ext-todo/tests/todo_e2e.rs`: Extension crate E2E coverage.

**Benchmarks:**
- `benches/agent_core.rs`: Agent core benchmark.
- `benches/tui_render.rs`: TUI render benchmark.
- `benches/soak.rs`: Long-session soak harness.
- `src/bin/tui-render-bench.rs`: Render benchmark binary.

## Naming Conventions

**Files:**
- `src/cli_<domain>.rs`: CLI command modules, for example `src/cli_provider.rs`, `src/cli_gateway.rs`, `src/cli_extension.rs`.
- `src/<domain>/mod.rs`: Domain module root, for example `src/agent/mod.rs`, `src/tools/mod.rs`.
- `src/<domain>/<surface>.rs`: One cohesive surface per file, for example `src/gateway/lane_router.rs`, `src/hooks/approval.rs`.
- `*_tests.rs` or `tests.rs`: Co-located module tests, for example `src/daemon/lifecycle_tests.rs`, `src/memory/tests.rs`.
- `CONTEXT.md`: Domain vocabulary docs beside important code, for example `src/gateway/CONTEXT.md`.

**Directories:**
- `src/<domain>/`: Runtime domain modules.
- `vulcan-ext-*`: First-party extension crates.
- `vulcan-core-ext-*`: Core extension crates linked by default binary.
- `docs/<category>/`: Documentation grouped by ADR, feature, guide, runtime, testing, and reference.

## Where to Add New Code

**New CLI Subcommand:**
- Primary code: Add enum shape to `src/cli.rs`, route in `vulcan/src/main.rs`, put implementation in `src/cli_<domain>.rs`.
- Tests: Add focused unit tests near the implementation or integration tests under `tests/`.

**New Agent Turn Behavior:**
- Primary code: Put shared behavior in `src/agent/turn.rs` or `src/agent/run.rs`; avoid divergent buffered/streaming branches.
- Tests: Use `src/agent/tests.rs` for unit-level behavior and `tests/agent_loop.rs` for end-to-end loop behavior.

**New Hook:**
- Primary code: Add a file under `src/hooks/`, implement `HookHandler`, wire it during `Agent::build_from_parts` in `src/agent/mod.rs` or through an extension crate.
- Tests: Co-locate hook tests in the new hook file or add integration coverage if it changes turn behavior.

**New Tool:**
- Primary code: Add `src/tools/<name>.rs`, expose it in `src/tools/mod.rs`, implement `Tool`, and register it in `ToolRegistry::new_with_diff_and_lsp`.
- Tests: Co-locate unit tests in the tool file; add agent-loop tests when model-visible dispatch behavior changes.

**New Provider Support:**
- Primary code: Extend `src/provider/factory.rs`, `src/provider/catalog.rs`, or add a provider module under `src/provider/` if the transport is not OpenAI-compatible.
- Tests: Add provider factory/catalog tests under `src/provider/` and CLI coverage in `src/cli_provider.rs` or `src/cli_model.rs`.

**New Daemon RPC Method:**
- Primary code: Add handler under `src/daemon/handlers/`, route method in `src/daemon/dispatch.rs`, update protocol/client helpers only when the wire shape changes.
- Tests: Use `src/daemon/protocol_tests.rs`, `src/daemon/lifecycle_tests.rs`, or `tests/daemon_e2e.rs`.

**New Gateway Route Or Connector:**
- Primary code: Add route under `src/gateway/routes/` and nest it in `src/gateway/server.rs`; add connector under `src/gateway/<platform>.rs` implementing `Platform` from `src/platform/mod.rs`.
- Tests: Add route tests in `src/gateway/server.rs` or connector tests in the connector module.

**New Extension:**
- Primary code: Add workspace member `vulcan-ext-<name>/`, implement session extension APIs from `src/extensions/api.rs`, link the crate in `vulcan/src/main.rs` when first-party/default.
- Tests: Use crate-local tests such as `vulcan-ext-todo/tests/`.

**Utilities:**
- Shared helpers: Prefer the owning domain module first; use root-level files like `src/context.rs`, `src/pause.rs`, or `src/observability.rs` only for cross-cutting primitives.

## Special Directories

**`.planning/codebase/`:**
- Purpose: Generated GSD codebase maps consumed by planner/executor workflows.
- Generated: Yes
- Committed: Yes

**`docs/plans/`:**
- Purpose: Local design/implementation plans for issue work.
- Generated: Sometimes
- Committed: Not generally; repo instructions treat many plan docs as local coordination artifacts.

**`Private/`:**
- Purpose: Private design handoff/reference assets.
- Generated: No
- Committed: Present in workspace; inspect only when task scope explicitly references it.

**`.fastembed_cache/`:**
- Purpose: Local model cache for embeddings.
- Generated: Yes
- Committed: No

**`target/`:**
- Purpose: Cargo build output.
- Generated: Yes
- Committed: No

**`.worktrees/`:**
- Purpose: Local Git worktrees for parallel branch work.
- Generated: Yes
- Committed: No

---

*Structure analysis: 2026-05-03*
