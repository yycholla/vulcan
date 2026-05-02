---
name: gateway
description: "Skill for the Gateway area of vulcan. 254 symbols across 23 files."
---

# Gateway

254 symbols | 23 files | Cohesion: 80%

## When to Use

- Working with code in `src/`
- Understanding how process_one, new, with_policy work
- Modifying gateway-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/gateway/queue.rs` | db_blocking, new, with_policy, enqueue, claim_next (+38) |
| `src/gateway/telegram.rs` | largest_photo_size, attachments_from_message, inbound_from_update_parts, inbound_from_update, inbound_from_webhook_body (+30) |
| `src/gateway/discord.rs` | inbound_from_message_parts, passes_allowlist, passes_mention_filter, message, inbound_from_message_parts_uses_channel_as_chat_id (+26) |
| `src/gateway/commands.rs` | run_builtin, help_text, status_text, clear, resume (+11) |
| `src/gateway/scheduler.rs` | spawn, run, fire, build_inbound_message_for_job, build_inbound_message_carries_job_fields (+10) |
| `src/gateway/outbound.rs` | abort, drop, new, with_poll_interval, fresh_db (+10) |
| `src/gateway/scheduler_store.rs` | as_str, new, conn, record_enqueued, record_skipped (+9) |
| `src/gateway/mod.rs` | run, run_on_listener, run_on_listener_with_parts, bind_addr, shutdown_signal (+9) |
| `src/gateway/registry.rs` | register, new, send, out_msg, registry_send_routes_by_platform_name (+5) |
| `src/gateway/stream_render.rs` | subsequent_chunk_uses_anchor_from_registry, new, handle, key, fresh_outbound (+5) |

## Entry Points

Start here when exploring this area:

- **`process_one`** (Function) — `src/gateway/worker.rs:41`
- **`new`** (Function) — `src/gateway/queue.rs:74`
- **`with_policy`** (Function) — `src/gateway/queue.rs:87`
- **`enqueue`** (Function) — `src/gateway/queue.rs:95`
- **`claim_next`** (Function) — `src/gateway/queue.rs:109`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `process_one` | Function | `src/gateway/worker.rs` | 41 |
| `new` | Function | `src/gateway/queue.rs` | 74 |
| `with_policy` | Function | `src/gateway/queue.rs` | 87 |
| `enqueue` | Function | `src/gateway/queue.rs` | 95 |
| `claim_next` | Function | `src/gateway/queue.rs` | 109 |
| `heartbeat` | Function | `src/gateway/queue.rs` | 143 |
| `mark_done` | Function | `src/gateway/queue.rs` | 156 |
| `complete_with_outbound` | Function | `src/gateway/queue.rs` | 164 |
| `mark_failed` | Function | `src/gateway/queue.rs` | 197 |
| `recover_processing` | Function | `src/gateway/queue.rs` | 239 |
| `count_dead` | Function | `src/gateway/queue.rs` | 256 |
| `list_dead` | Function | `src/gateway/queue.rs` | 270 |
| `replay_dead` | Function | `src/gateway/queue.rs` | 300 |
| `as_str` | Function | `src/gateway/scheduler_store.rs` | 42 |
| `new` | Function | `src/gateway/scheduler_store.rs` | 72 |
| `record_enqueued` | Function | `src/gateway/scheduler_store.rs` | 85 |
| `record_skipped` | Function | `src/gateway/scheduler_store.rs` | 105 |
| `record_enqueue_failed` | Function | `src/gateway/scheduler_store.rs` | 124 |
| `get` | Function | `src/gateway/scheduler_store.rs` | 143 |
| `list` | Function | `src/gateway/scheduler_store.rs` | 171 |

## Execution Flows

| Flow | Type | Steps |
|------|------|-------|
| `Post_inbound_db_error_returns_503 → Apply_connection_pragmas` | cross_community | 8 |
| `Post_inbound_enqueues_and_returns_id → Apply_connection_pragmas` | cross_community | 8 |
| `Post_inbound_missing_bearer_returns_401 → Apply_connection_pragmas` | cross_community | 8 |
| `Worker_routes_slash_help_through_dispatcher → Connect` | cross_community | 8 |
| `Post_inbound_db_error_returns_503 → Dirs_or_default` | cross_community | 7 |
| `Worker_routes_slash_help_through_dispatcher → Dirs_or_default` | cross_community | 7 |
| `Webhook_loopback_accepts_signed_request → Apply_connection_pragmas` | cross_community | 5 |
| `Get_scheduler_merges_run_history → Apply_connection_pragmas` | cross_community | 5 |
| `Get_lanes_empty_cache_returns_empty_array → Apply_connection_pragmas` | cross_community | 5 |
| `Webhook_loopback_rejects_invalid_signature → Apply_connection_pragmas` | cross_community | 5 |

## Connected Areas

| Area | Connections |
|------|-------------|
| Daemon | 8 calls |
| Code | 7 calls |
| Config | 6 calls |
| Memory | 4 calls |
| Tui | 2 calls |
| Agent | 1 calls |
| Routes | 1 calls |
| State | 1 calls |

## How to Explore

1. `gitnexus_context({name: "process_one"})` — see callers and callees
2. `gitnexus_query({query: "gateway"})` — find related execution flows
3. Read key files listed above for implementation details
