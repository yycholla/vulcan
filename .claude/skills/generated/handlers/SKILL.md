---
name: handlers
description: "Skill for the Handlers area of vulcan. 62 symbols across 13 files."
---

# Handlers

62 symbols | 13 files | Cohesion: 75%

## When to Use

- Working with code in `src/`
- Understanding how cortex, config, queue_reload work
- Modifying handlers-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/daemon/handlers/session.rs` | search, resume, history, create, destroy (+12) |
| `src/daemon/handlers/cortex.rs` | store, search, stats, recall, seed (+6) |
| `src/daemon/state.rs` | cortex, config, queue_reload, new, for_tests_minimal (+2) |
| `src/daemon/handlers/prompt.rs` | stream_event_to_frame, run, stream, run_returns_session_not_found_for_bogus_session, run_attempts_lazy_build_for_session_without_agent |
| `src/daemon/handlers/daemon_ops.rs` | shutdown, reload, status, ping, ping_returns_response_with_pong |
| `src/daemon/config_watch.rs` | start, write_and_settle, reload_applies_when_idle, reload_deferred_while_session_in_flight, rapid_edits_coalesce |
| `src/daemon/handlers/agent.rs` | resolve, status, switch_model, list_models |
| `src/daemon/protocol_tests.rs` | response_error_shape, response_error_round_trips_through_json |
| `src/daemon/handlers/approval.rs` | pending, respond |
| `src/daemon/session.rs` | touch |

## Entry Points

Start here when exploring this area:

- **`cortex`** (Function) — `src/daemon/state.rs:63`
- **`config`** (Function) — `src/daemon/state.rs:70`
- **`queue_reload`** (Function) — `src/daemon/state.rs:130`
- **`touch`** (Function) — `src/daemon/session.rs:79`
- **`error`** (Function) — `src/daemon/protocol.rs:84`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `cortex` | Function | `src/daemon/state.rs` | 63 |
| `config` | Function | `src/daemon/state.rs` | 70 |
| `queue_reload` | Function | `src/daemon/state.rs` | 130 |
| `touch` | Function | `src/daemon/session.rs` | 79 |
| `error` | Function | `src/daemon/protocol.rs` | 84 |
| `dispatch` | Function | `src/daemon/dispatch.rs` | 31 |
| `search` | Function | `src/daemon/handlers/session.rs` | 118 |
| `resume` | Function | `src/daemon/handlers/session.rs` | 130 |
| `history` | Function | `src/daemon/handlers/session.rs` | 142 |
| `run` | Function | `src/daemon/handlers/prompt.rs` | 97 |
| `stream` | Function | `src/daemon/handlers/prompt.rs` | 139 |
| `shutdown` | Function | `src/daemon/handlers/daemon_ops.rs` | 22 |
| `reload` | Function | `src/daemon/handlers/daemon_ops.rs` | 27 |
| `status` | Function | `src/daemon/handlers/daemon_ops.rs` | 32 |
| `store` | Function | `src/daemon/handlers/cortex.rs` | 16 |
| `search` | Function | `src/daemon/handlers/cortex.rs` | 42 |
| `stats` | Function | `src/daemon/handlers/cortex.rs` | 77 |
| `recall` | Function | `src/daemon/handlers/cortex.rs` | 111 |
| `seed` | Function | `src/daemon/handlers/cortex.rs` | 146 |
| `edges_from` | Function | `src/daemon/handlers/cortex.rs` | 165 |

## Execution Flows

| Flow | Type | Steps |
|------|------|-------|
| `Seed → Apply_connection_pragmas` | cross_community | 8 |
| `Destroy_fires_agent_cancel_token_when_present → New` | cross_community | 7 |
| `Cancel_fires_token_and_reports_in_flight_state → New` | cross_community | 7 |
| `Cancel_fires_token_and_reports_in_flight_state → New` | cross_community | 7 |
| `Cancel_reports_false_when_not_in_flight → New` | cross_community | 7 |
| `Cancel_reports_false_when_not_in_flight → New` | cross_community | 7 |
| `Seed → Dirs_or_default` | cross_community | 7 |
| `Worker_marks_inbound_failed_when_daemon_agent_build_fails → New` | cross_community | 7 |
| `Worker_marks_inbound_failed_when_daemon_agent_build_fails → New` | cross_community | 7 |
| `List_reflects_create_destroy → New` | cross_community | 7 |

## Connected Areas

| Area | Connections |
|------|-------------|
| Daemon | 18 calls |
| Agent | 3 calls |
| Memory | 2 calls |
| Hooks | 1 calls |
| Provider | 1 calls |

## How to Explore

1. `gitnexus_context({name: "cortex"})` — see callers and callees
2. `gitnexus_query({query: "handlers"})` — find related execution flows
3. Read key files listed above for implementation details
