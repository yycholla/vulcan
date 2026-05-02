---
name: daemon
description: "Skill for the Daemon area of vulcan. 143 symbols across 21 files."
---

# Daemon

143 symbols | 21 files | Cohesion: 73%

## When to Use

- Working with code in `src/`
- Understanding how with_client_factory, snapshot_cache, signal_shutdown work
- Modifying daemon-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/daemon/session.rs` | agent_cancel, get, ids, with_main, create_named (+22) |
| `src/daemon/protocol_tests.rs` | frame_round_trip_request, frame_round_trip_response, frame_round_trip_stream_frame, multiple_frames_round_trip_in_order, oversized_frame_rejected_before_alloc (+7) |
| `src/daemon/server.rs` | bind, run, handle_connection, ping, server_responds_to_ping (+6) |
| `src/daemon/dispatch.rs` | new, req, ping_dispatches_to_daemon_ops, unknown_method_returns_unknown_method_error, shutdown_signals_state (+6) |
| `src/daemon/lifecycle_tests.rs` | pid_file_create_excl_rejects_second_writer, pid_file_released_on_drop, pid_file_writes_current_pid, pid_file_perms_are_0600, pid_file_acquire_or_replace_stale_overwrites_dead_pid (+6) |
| `src/daemon/eviction.rs` | evict_idle, make_idle, evicts_idle_non_main_session, does_not_evict_main, does_not_evict_in_flight_session (+5) |
| `src/daemon/cli.rs` | start, install_signal_handlers, spawn_detached, install, run (+5) |
| `src/daemon/protocol.rs` | write_frame_bytes, read_frame_bytes, write_request, read_request, write_response (+4) |
| `src/daemon/install.rs` | render_unit, systemd_user_unit_dir, write_systemd_unit, install_systemd_default, render_unit_contains_required_sections (+3) |
| `src/gateway/lane_router.rs` | with_client_factory, snapshot_cache, lane, derive_session_id_is_stable, ensure_session_creates_and_caches (+2) |

## Entry Points

Start here when exploring this area:

- **`with_client_factory`** (Function) — `src/gateway/lane_router.rs:68`
- **`snapshot_cache`** (Function) — `src/gateway/lane_router.rs:141`
- **`signal_shutdown`** (Function) — `src/daemon/state.rs:114`
- **`bind`** (Function) — `src/daemon/server.rs:30`
- **`run`** (Function) — `src/daemon/server.rs:48`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `with_client_factory` | Function | `src/gateway/lane_router.rs` | 68 |
| `snapshot_cache` | Function | `src/gateway/lane_router.rs` | 141 |
| `signal_shutdown` | Function | `src/daemon/state.rs` | 114 |
| `bind` | Function | `src/daemon/server.rs` | 30 |
| `run` | Function | `src/daemon/server.rs` | 48 |
| `write_frame_bytes` | Function | `src/daemon/protocol.rs` | 154 |
| `read_frame_bytes` | Function | `src/daemon/protocol.rs` | 169 |
| `write_request` | Function | `src/daemon/protocol.rs` | 184 |
| `read_request` | Function | `src/daemon/protocol.rs` | 190 |
| `write_response` | Function | `src/daemon/protocol.rs` | 196 |
| `read_response` | Function | `src/daemon/protocol.rs` | 206 |
| `write_stream_frame` | Function | `src/daemon/protocol.rs` | 212 |
| `read_stream_frame` | Function | `src/daemon/protocol.rs` | 224 |
| `connect` | Function | `src/client/transport.rs` | 32 |
| `call` | Function | `src/client/transport.rs` | 42 |
| `call_stream` | Function | `src/client/transport.rs` | 53 |
| `connect_at` | Function | `src/client/mod.rs` | 44 |
| `handle` | Function | `src/gateway/routes/lanes.rs` | 14 |
| `with_cortex` | Function | `src/daemon/state.rs` | 57 |
| `sessions` | Function | `src/daemon/state.rs` | 100 |

## Execution Flows

| Flow | Type | Steps |
|------|------|-------|
| `Webhook_loopback_accepts_signed_request → Connect` | cross_community | 9 |
| `Post_inbound_enqueues_and_returns_id → Connect` | cross_community | 9 |
| `Webhook_loopback_rejects_invalid_signature → Connect` | cross_community | 9 |
| `Webhook_rejects_oversized_body_before_verification → Connect` | cross_community | 9 |
| `Post_inbound_unknown_platform_returns_400 → Connect` | cross_community | 9 |
| `Post_inbound_missing_bearer_returns_401 → Connect` | cross_community | 9 |
| `Webhook_loopback_accepts_signed_request → Dirs_or_default` | cross_community | 8 |
| `Get_scheduler_merges_run_history → Connect` | cross_community | 8 |
| `Get_lanes_empty_cache_returns_empty_array → Connect` | cross_community | 8 |
| `Post_inbound_enqueues_and_returns_id → Dirs_or_default` | cross_community | 8 |

## Connected Areas

| Area | Connections |
|------|-------------|
| Handlers | 52 calls |
| Gateway | 10 calls |
| Agent | 4 calls |
| Impact | 2 calls |
| Config | 2 calls |
| Client | 1 calls |
| Routes | 1 calls |
| Policy | 1 calls |

## How to Explore

1. `gitnexus_context({name: "with_client_factory"})` — see callers and callees
2. `gitnexus_query({query: "daemon"})` — find related execution flows
3. Read key files listed above for implementation details
