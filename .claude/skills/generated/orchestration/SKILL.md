---
name: orchestration
description: "Skill for the Orchestration area of vulcan. 33 symbols across 3 files."
---

# Orchestration

33 symbols | 3 files | Cohesion: 78%

## When to Use

- Working with code in `src/`
- Understanding how is_terminal, update_status, update_phase work
- Modifying orchestration-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/orchestration/mod.rs` | is_terminal, update_status, update_phase, update_iterations, update_tokens (+24) |
| `src/tui/state/tests.rs` | subagent_tiles_include_child_records_from_store, tree_nodes_include_child_records_from_store, delegated_worker_count_filters_terminal_records |
| `src/tui/state/mod.rs` | delegated_worker_count |

## Entry Points

Start here when exploring this area:

- **`is_terminal`** (Function) — `src/orchestration/mod.rs:76`
- **`update_status`** (Function) — `src/orchestration/mod.rs:194`
- **`update_phase`** (Function) — `src/orchestration/mod.rs:206`
- **`update_iterations`** (Function) — `src/orchestration/mod.rs:215`
- **`update_tokens`** (Function) — `src/orchestration/mod.rs:226`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `is_terminal` | Function | `src/orchestration/mod.rs` | 76 |
| `update_status` | Function | `src/orchestration/mod.rs` | 194 |
| `update_phase` | Function | `src/orchestration/mod.rs` | 206 |
| `update_iterations` | Function | `src/orchestration/mod.rs` | 215 |
| `update_tokens` | Function | `src/orchestration/mod.rs` | 226 |
| `mark_completed` | Function | `src/orchestration/mod.rs` | 237 |
| `mark_failed` | Function | `src/orchestration/mod.rs` | 255 |
| `mark_cancelled` | Function | `src/orchestration/mod.rs` | 268 |
| `list` | Function | `src/orchestration/mod.rs` | 286 |
| `cancel` | Function | `src/orchestration/mod.rs` | 320 |
| `delegated_worker_count` | Function | `src/tui/state/mod.rs` | 768 |
| `register` | Function | `src/orchestration/mod.rs` | 162 |
| `get` | Function | `src/orchestration/mod.rs` | 279 |
| `recent` | Function | `src/orchestration/mod.rs` | 292 |
| `register_cancel_handle` | Function | `src/orchestration/mod.rs` | 302 |
| `forget_cancel_handle` | Function | `src/orchestration/mod.rs` | 310 |
| `children_of` | Function | `src/orchestration/mod.rs` | 338 |
| `with_mut` | Function | `src/orchestration/mod.rs` | 356 |
| `lifecycle_transitions` | Function | `src/orchestration/mod.rs` | 392 |
| `terminal_records_are_immutable` | Function | `src/orchestration/mod.rs` | 412 |

## Execution Flows

| Flow | Type | Steps |
|------|------|-------|
| `Tree_of_thought → Is_terminal` | cross_community | 4 |

## Connected Areas

| Area | Connections |
|------|-------------|
| State | 3 calls |

## How to Explore

1. `gitnexus_context({name: "is_terminal"})` — see callers and callees
2. `gitnexus_query({query: "orchestration"})` — find related execution flows
3. Read key files listed above for implementation details
