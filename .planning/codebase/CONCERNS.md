---
title: CONCERNS
last_mapped_commit: f5e07ca31dd7a4ac794b9838884bfdad74a7e37c
mapped_at: 2026-05-03
scope: full repo
---

# Concerns

This document records current technical debt, drift risks, and fragile areas discovered during the map pass.

## Documentation Drift

- `README.md` still describes some direct CLI usage while ADRs like `docs/adr/0001-daemon-required-frontends.md` establish daemon-required frontend direction.
- Some paths still carry historical `ferris` naming in docs or config comments; repo guidance says to fix those only when touching nearby files.
- Per-module context docs are useful but unevenly detailed, especially for fast-moving modules such as `src/agent/`, `src/provider/`, and `src/tools/`.
- ADR numbering has a duplicate `0007`: `docs/adr/0007-extension-frontend-events-and-status-widgets.md` and `docs/adr/0007-symphony-workflow-contract.md`.

## Runtime Boundary Drift

- The desired architecture puts frontends behind a daemon-owned runtime pool.
- Current command paths in `vulcan/src/main.rs` may still include direct fallback behavior for some one-shot operations.
- Any new CLI/TUI work should verify whether it is using daemon client paths or accidentally recreating direct runtime behavior.
- Symphony should consume base Vulcan daemon/tool/observability capabilities rather than growing bespoke copies.

## Hook And Provider Complexity

- Hook behavior must stay aligned across buffered and streaming provider paths.
- Tool hooks can replace arguments and results, so tests need to cover both normal and hook-altered flows.
- Provider sanitization in `src/provider/think_sanitizer.rs` is sensitive to vendor-specific response formats.
- Redaction in `src/provider/redact.rs` must keep pace with new provider metadata and observability attributes.

## Observability Maturity

- Observability is new and centralized in `src/observability.rs`.
- Config supports traces, metrics, export interval, service name, and surface toggles, but coverage should be audited as new surfaces are added.
- TUI performance metrics, daemon request boundaries, provider requests, tool calls, hook events, and process metrics should use a consistent metric vocabulary.
- SigNoz/OTLP behavior should be tested against real collector exports and local disabled-by-default behavior.

## CI And Lint Debt

- `.github/workflows/ci.yml` sets `RUSTFLAGS=-D warnings`.
- Clippy currently runs with `continue-on-error`, which means warning debt may be known but not blocking.
- Common warning classes noted in prior runs include collapsible conditionals, argument count, and needless borrow patterns.
- Cargo feature interactions are broad because `gateway`, `telegram`, daemon, extensions, and optional benchmark dependencies overlap.

## Compile Time And Dependency Surface

- The workspace pulls in heavy dependencies for TUI, gateway, tree-sitter, embeddings, OpenTelemetry, and SQLite.
- Feature boundaries should be guarded so optional gateway or embedding dependencies do not leak into smaller builds.
- Extension crates add useful modularity but also increase workspace graph size.
- Benchmark and test workflows should catch compile-time regressions where possible.

## Gateway And Daemon Reliability

- Gateway lanes, schedulers, queues, and platform connectors involve long-lived state in `src/gateway/`.
- Daemon lifecycle, eviction, config-watch, session agents, and subagents are spread across multiple modules in `src/daemon/`.
- Queue durability and session mapping bugs can become user-visible across Discord, Telegram, loopback, and TUI surfaces.
- Gateway auth and webhook handling should remain explicit and tested when new platform routes are added.

## Extension System Risk

- Extension architecture spans daemon lifecycle, frontend rendering, manifests, policy, verification, status widgets, and events.
- Dynamic distribution/lifecycle decisions are documented in ADRs but may not all be fully implemented.
- Frontend extension rendering must remain replayable and version-aware.
- Policy checks should avoid granting broad tool or filesystem capability by default.

## Workflow Tooling Gaps

- `$gsd-map-codebase` expected `gsd-sdk`, but the current shell did not have that command available.
- The dedicated `gsd-codebase-mapper` runtime agent was not available in this Codex session, so this map used the documented sequential fallback.
- If GSD automation becomes a repo requirement, bootstrap instructions should be added near `docs/agents/` or project setup docs.
- Current worktree had unrelated untracked files during this map; commits should remain carefully scoped.
