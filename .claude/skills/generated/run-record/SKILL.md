---
name: run-record
description: "Skill for the Run_record area of vulcan. 39 symbols across 4 files."
---

# Run_record

39 symbols | 4 files | Cohesion: 72%

## When to Use

- Working with code in `src/`
- Understanding how of, dispatch_tool, as_str work
- Modifying run_record-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/run_record/mod.rs` | create, append_event, finalize, in_memory_store_records_full_lifecycle, in_memory_store_caps_records (+20) |
| `src/cli_run.rs` | resolve_run_id, resolve_run_id_accepts_full_uuid, resolve_run_id_accepts_prefix, resolve_run_id_errors_on_no_match, format_event (+5) |
| `src/cli_replay.rs` | run, inspect, resolve |
| `src/agent/dispatch.rs` | dispatch_tool |

## Entry Points

Start here when exploring this area:

- **`of`** (Function) — `src/run_record/mod.rs:114`
- **`dispatch_tool`** (Function) — `src/agent/dispatch.rs:22`
- **`as_str`** (Function) — `src/run_record/mod.rs:126`
- **`try_open_at`** (Function) — `src/run_record/mod.rs:333`
- **`try_open_in_memory`** (Function) — `src/run_record/mod.rs:343`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `of` | Function | `src/run_record/mod.rs` | 114 |
| `dispatch_tool` | Function | `src/agent/dispatch.rs` | 22 |
| `as_str` | Function | `src/run_record/mod.rs` | 126 |
| `try_open_at` | Function | `src/run_record/mod.rs` | 333 |
| `try_open_in_memory` | Function | `src/run_record/mod.rs` | 343 |
| `run` | Function | `src/cli_run.rs` | 12 |
| `try_new` | Function | `src/run_record/mod.rs` | 326 |
| `run` | Function | `src/cli_replay.rs` | 8 |
| `from_uuid` | Function | `src/run_record/mod.rs` | 46 |
| `new` | Function | `src/run_record/mod.rs` | 42 |
| `new` | Function | `src/run_record/mod.rs` | 224 |
| `new` | Function | `src/run_record/mod.rs` | 266 |
| `resolve_run_id` | Function | `src/cli_run.rs` | 75 |
| `resolve_run_id_accepts_full_uuid` | Function | `src/cli_run.rs` | 180 |
| `resolve_run_id_accepts_prefix` | Function | `src/cli_run.rs` | 190 |
| `resolve_run_id_errors_on_no_match` | Function | `src/cli_run.rs` | 201 |
| `create` | Function | `src/run_record/mod.rs` | 275 |
| `append_event` | Function | `src/run_record/mod.rs` | 284 |
| `finalize` | Function | `src/run_record/mod.rs` | 295 |
| `in_memory_store_records_full_lifecycle` | Function | `src/run_record/mod.rs` | 547 |

## Execution Flows

| Flow | Type | Steps |
|------|------|-------|
| `Run → StoreInner` | cross_community | 5 |
| `Run → KeyParseError` | cross_community | 5 |
| `Run → Next` | cross_community | 5 |
| `Run → Dirs_or_default` | cross_community | 4 |
| `Run → Initialize` | cross_community | 4 |
| `Run → From_uuid` | cross_community | 4 |
| `Run → Get` | cross_community | 4 |
| `Run → RunRecord` | intra_community | 4 |

## Connected Areas

| Area | Connections |
|------|-------------|
| Config | 3 calls |
| Tui | 2 calls |
| Replay | 1 calls |

## How to Explore

1. `gitnexus_context({name: "of"})` — see callers and callees
2. `gitnexus_query({query: "run_record"})` — find related execution flows
3. Read key files listed above for implementation details
