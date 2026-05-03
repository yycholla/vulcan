<!-- generated-by: gsd-doc-writer -->
# Runtime Modules

This document fills the runtime-module documentation gap for source areas that are not covered by the canonical architecture, configuration, API, and testing docs.

## Overview

The runtime is split into modules that keep agent execution, daemon state, gateway delivery, local policy, knowledge indexing, and terminal rendering separate. Most modules are wired through the root library in `src/lib.rs`.

## Module Listing

| Module | Purpose |
|--------|---------|
| `src/artifact/` | Typed artifacts persisted alongside saved runs. |
| `src/client/` | Client protocol used by CLI/TUI/gateway code to reach the daemon. |
| `src/context_pack/` | Named context bundles used to brief agent tasks. |
| `src/doctor/` | Structured diagnostics for config, storage, workspace, and runtime health. |
| `src/impact/` | File impact reporting for code references, tests, and docs. |
| `src/knowledge/` | Local knowledge and indexing command surfaces. |
| `src/memory/` | SQLite/redb-backed persistence for sessions, queues, scheduler data, and Cortex state. |
| `src/orchestration/` | Coordination support for multi-step agent workflows. |
| `src/platform/` | Platform abstraction shared by gateway connectors. |
| `src/playbook/` | Project playbook management. |
| `src/policy/` | Workspace trust and tool-policy simulation. |
| `src/release/` | Release summary rendering for git revision ranges. |
| `src/replay/` | Saved-run replay and inspection support. |
| `src/review/` | Read-only critic/review command support. |
| `src/run_record/` | Durable run timeline records. |
| `src/snapshots/` | Snapshot-related support. |
| `src/symphony/` | Workspace/task orchestration and runner support. |
| `src/trust/` | Workspace trust resolution. |

## Key Interfaces

- `Config` in `src/config/mod.rs` is the shared configuration root.
- `ConfigField` in `src/config_registry.rs` describes user-facing config keys for `vulcan config`.
- `DaemonState` in `src/daemon/state.rs` owns daemon runtime state.
- `RuntimeResourcePool` in `src/daemon/resource_pool.rs` centralizes reusable runtime resources.
- `ToolProfile` in `src/tools/profile.rs` describes tool capability profiles.

## Usage Notes

When adding runtime behavior, prefer the existing module boundary closest to the capability. For example, config schema belongs in `src/config/` and `src/config_registry.rs`, runtime diagnostics belong in `src/doctor/`, and long-lived backend state belongs in `src/daemon/`.
