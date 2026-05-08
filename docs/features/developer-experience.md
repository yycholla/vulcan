---
title: Developer Experience (DX) & Tooling
type: feature
created: 2026-05-14
tags: [extensions, cli, testing, templates]
---

# Developer Experience (DX) & Tooling

Make building, testing, and distributing extensions as easy as possible.

## Extension CLI

A unified CLI for the extension lifecycle.

- `vulcan extension new <name>` — Scaffold a new extension (choose runtime class: wasm for third-party managed code, subprocess for local scripts/tools, mcp for protocol bridges, or native_first_party for trusted internal cargo-crate work). Generates manifest, stub runtime entry, CI template, README.
- `vulcan extension build` — Build for selected target(s); produce `.vpk` packages.
- `vulcan extension test` — Spin up an isolated sandbox agent and run integration tests against extension hooks.
- `vulcan extension run` — Run extension in local dev agent with hot reload (watch and reload on changes).
- `vulcan extension publish` — Sign, package, and upload to a repository.
- `vulcan extension install <id>` and `vulcan extension uninstall <id>` — Local package management.

## Mock Agent Contexts (Testing Harness)

A test harness that implements `ExtensionContext` for unit and integration testing.

- Fake tool registry: mock tool implementations with programmable responses.
- In-memory event bus: capture events and assert on them.
- Memory backend mocks: deterministic behavior for storage/retrieval.

## Live Debugging & Hot Reload

- **WASM**: Restart Wasmtime instance without restarting the host process; preserve minimal state across reloads.
- **Native first-party (dev mode)**: Allow trusted internal cargo-crate extension debugging. Loading debug `.so`/`.dylib` from `target/debug/` remains an internal experiment only, not a marketplace or third-party path.
- **Scripting**: Reload subprocess adapter commands or WASM components; generalized in-process JS/Python embedding is deferred.
- **Attach debugger**: Debuginfo for native extensions; console + simple step-through UI for WASM.

## Templates & Starters

Curated starter templates:

- `memory-backend` — Custom memory backend (Redis, PostgreSQL, local file).
- `custom-tool` — Wrap an HTTP API or CLI as an agent tool.
- `rag-pipeline` — Ingest → chunk → embed → vector store, exposed as tools.
- `event-logger` — Hook into events and export to OpenTelemetry.
- `approval-gate` — Policy-based approval workflow.

---

## Example: `vulcan extension new` Output

```bash
$ vulcan extension new my-tool --target wasm
Created:
  extension.toml
  src/lib.rs (WASM stub with init() and register_tool)
  tests/integration.rs
  .github/workflows/release.yml
  README.md

Next steps:
  cd my-tool
  vulcan extension build
  vulcan extension test
  vulcan extension publish
```
