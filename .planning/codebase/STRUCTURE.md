---
title: STRUCTURE
last_mapped_commit: f5e07ca31dd7a4ac794b9838884bfdad74a7e37c
mapped_at: 2026-05-03
scope: full repo
---

# Structure

The repository is a single Rust workspace with most product logic in the root crate and focused companion crates for the binary, frontend API, TUI support, macros, and extensions.

## Top Level

- `Cargo.toml` declares the workspace, root package, dependencies, features, profiles, tests, and benchmarks.
- `Cargo.lock` pins dependency resolution.
- `README.md` describes user-facing commands and capabilities.
- `CONTEXT.md` and `CONTEXT-MAP.md` describe the domain model and navigation map for agents.
- `config.example.toml` is the primary configuration reference.
- `rustfmt.toml`, `clippy.toml`, and `deny.toml` define repository quality tooling.
- `.github/workflows/ci.yml` and `.github/workflows/bench.yml` define automation.

## Core Source Layout

- `src/agent/` contains the long-lived agent, turn runner, dispatch, provider wiring, session model, and tests.
- `src/daemon/` contains lifecycle, protocol, server, session, subagent, eviction, config-watch, and daemon state modules.
- `src/provider/` contains provider abstraction, OpenAI-compatible transport, catalog, factory, mocks, redaction, and think-tag sanitization.
- `src/tools/` contains tool registry and concrete tool implementations.
- `src/hooks/` contains the hook registry, outcomes, and built-in handlers.
- `src/tui/` contains terminal UI state, rendering, events, surfaces, themes, pickers, prompt editor, and runtime glue.
- `src/gateway/` contains gateway server, queues, lanes, platform connectors, rendering, workers, and scheduler.
- `src/extensions/` contains extension APIs, manifest parsing, registry, policy, lifecycle state, and verification.
- `src/symphony/` contains workflow, task-source, config, app-server client, workspace, runner, and orchestrator code.

## Supporting Domains

- `src/client/` contains daemon client transport and autostart behavior.
- `src/code/` contains code graph, embedding, and repository intelligence helpers.
- `src/memory/` contains memory codec, schema, cortex integration, and tests.
- `src/knowledge/`, `src/context_pack/`, and `src/playbook/` contain higher-level local knowledge features.
- `src/review/`, `src/replay/`, `src/run_record/`, `src/impact/`, and `src/trust/` support agent workflow and auditability.
- `src/platform/`, `src/orchestration/`, `src/release/`, and `src/policy/` contain additional platform and workflow surfaces.
- `src/observability.rs` centralizes telemetry setup.

## Binary And Workspace Crates

- `vulcan/` contains the binary crate and `vulcan/src/main.rs`.
- `vulcan-frontend-api/` contains shared frontend extension contracts.
- `vulcan-tui/` contains TUI support crate code.
- `vulcan-extension-macros/` provides extension-related procedural macros.
- `vulcan-core-ext-skills/` and `vulcan-core-ext-safety/` are core extension crates.
- `vulcan-ext-*` crates are first-party extension implementations and demos.

## Docs

- Architecture decisions live in `docs/adr/`.
- Feature docs live in `docs/features/`.
- Agent workflow docs live under `docs/agents/`.
- Design references and user-supplied assets may appear under `docs/reference/`.
- Per-domain context docs live beside code, for example `src/hooks/CONTEXT.md`, `src/tui/CONTEXT.md`, and `src/symphony/CONTEXT.md`.

## Tests And Benchmarks

- Root integration tests live in `tests/`.
- Module tests are colocated, for example `src/config/tests.rs`, `src/daemon/lifecycle_tests.rs`, and `src/memory/tests.rs`.
- Extension E2E tests can live inside extension crates, for example `vulcan-ext-todo/tests/todo_e2e.rs`.
- Benchmark binaries and harnesses include `src/bin/tui-render-bench.rs` and workflow coverage in `.github/workflows/bench.yml`.

## Naming Patterns

- CLI modules use `src/cli_<domain>.rs`.
- Domain modules usually use `src/<domain>/mod.rs`.
- Context docs use `CONTEXT.md` at root and within significant module directories.
- Extension crates use `vulcan-ext-*` or `vulcan-core-ext-*`.
- Tests use direct domain names and E2E suffixes, such as `tests/daemon_e2e.rs`.
