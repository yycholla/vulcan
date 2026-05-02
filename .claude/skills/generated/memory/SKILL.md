---
name: memory
description: "Skill for the Memory area of vulcan. 81 symbols across 8 files."
---

# Memory

81 symbols | 8 files | Cohesion: 74%

## When to Use

- Working with code in `src/`
- Understanding how run, seed_from_sessions, new work
- Modifying memory-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/cli_cortex.rs` | run, seed_from_sessions, open_cortex, cmd_search, cmd_stats (+24) |
| `src/memory/tests.rs` | store_in, round_trip_messages, provider_profile_round_trips, provider_profile_survives_save_messages, list_sessions_includes_provider_profile (+12) |
| `src/memory/cortex.rs` | get_node, list_nodes, edges_from, search, traverse (+11) |
| `src/memory/mod.rs` | new, load_history, save_messages, save_provider_profile, save_session_metadata (+4) |
| `src/memory/schema.rs` | upsert_session_metadata, upsert_session_provider_profile, apply_connection_pragmas, initialize_conn, open_gateway_pool |
| `src/memory/codec.rs` | decode_message, encode_message |
| `src/agent/mod.rs` | replace_history, save_messages |
| `src/config/mod.rs` | default_enabled |

## Entry Points

Start here when exploring this area:

- **`run`** (Function) — `src/cli_cortex.rs:23`
- **`seed_from_sessions`** (Function) — `src/cli_cortex.rs:182`
- **`new`** (Function) — `src/memory/mod.rs:120`
- **`get_node`** (Function) — `src/memory/cortex.rs:105`
- **`list_nodes`** (Function) — `src/memory/cortex.rs:110`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `run` | Function | `src/cli_cortex.rs` | 23 |
| `seed_from_sessions` | Function | `src/cli_cortex.rs` | 182 |
| `new` | Function | `src/memory/mod.rs` | 120 |
| `get_node` | Function | `src/memory/cortex.rs` | 105 |
| `list_nodes` | Function | `src/memory/cortex.rs` | 110 |
| `edges_from` | Function | `src/memory/cortex.rs` | 142 |
| `search` | Function | `src/memory/cortex.rs` | 187 |
| `traverse` | Function | `src/memory/cortex.rs` | 192 |
| `stats` | Function | `src/memory/cortex.rs` | 222 |
| `config` | Function | `src/memory/cortex.rs` | 246 |
| `default_enabled` | Function | `src/config/mod.rs` | 592 |
| `seed_from_sessions_to` | Function | `src/cli_cortex.rs` | 189 |
| `upsert_session_metadata` | Function | `src/memory/schema.rs` | 230 |
| `upsert_session_provider_profile` | Function | `src/memory/schema.rs` | 252 |
| `load_history` | Function | `src/memory/mod.rs` | 140 |
| `save_messages` | Function | `src/memory/mod.rs` | 206 |
| `save_provider_profile` | Function | `src/memory/mod.rs` | 293 |
| `save_session_metadata` | Function | `src/memory/mod.rs` | 322 |
| `list_sessions` | Function | `src/memory/mod.rs` | 335 |
| `decode_message` | Function | `src/memory/codec.rs` | 51 |

## Execution Flows

| Flow | Type | Steps |
|------|------|-------|
| `Post_inbound_db_error_returns_503 → Apply_connection_pragmas` | cross_community | 8 |
| `Post_inbound_enqueues_and_returns_id → Apply_connection_pragmas` | cross_community | 8 |
| `Seed → Apply_connection_pragmas` | cross_community | 8 |
| `Post_inbound_missing_bearer_returns_401 → Apply_connection_pragmas` | cross_community | 8 |
| `Post_inbound_db_error_returns_503 → Dirs_or_default` | cross_community | 7 |
| `Seed → Dirs_or_default` | cross_community | 7 |
| `Recall_hook_injects_when_fts_returns_hits → Apply_connection_pragmas` | cross_community | 7 |
| `Agent_create_artifact_persists_with_run_and_session_links → Apply_connection_pragmas` | cross_community | 6 |
| `Recall_hook_injects_when_fts_returns_hits → Dirs_or_default` | cross_community | 6 |
| `Webhook_loopback_accepts_signed_request → Apply_connection_pragmas` | cross_community | 5 |

## Connected Areas

| Area | Connections |
|------|-------------|
| Config | 5 calls |
| Hooks | 2 calls |
| State | 1 calls |
| Client | 1 calls |
| Tui | 1 calls |
| Code | 1 calls |

## How to Explore

1. `gitnexus_context({name: "run"})` — see callers and callees
2. `gitnexus_query({query: "memory"})` — find related execution flows
3. Read key files listed above for implementation details
