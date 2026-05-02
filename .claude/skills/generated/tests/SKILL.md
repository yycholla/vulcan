---
name: tests
description: "Skill for the Tests area of vulcan. 36 symbols across 9 files."
---

# Tests

36 symbols | 9 files | Cohesion: 91%

## When to Use

- Working with code in `tests/`
- Understanding how builtin_profile work
- Modifying tests-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `tests/contracts.rs` | tool_names, readonly_profile_does_not_expose_mutating_tools, gateway_safe_profile_blocks_all_workspace_mutation, default_registry_contains_foundational_tools, chat (+2) |
| `tests/client_autostart.rs` | vulcan_with_home, wait_for_socket_gone, cold_invocation_autostarts_daemon, second_invocation_reuses_daemon, autostart_handles_stale_socket (+2) |
| `tests/daemon_e2e.rs` | vulcan_with_home, daemon_start_detach_status_stop, daemon_status_fails_when_no_daemon, daemon_socket_is_0600, daemon_pid_file_is_0600 |
| `src/tools/profile.rs` | gateway_safe_profile, builtin_profile, gateway_safe_allows_no_workspace_mutation |
| `tests/agent_loop.rs` | chat, chat_stream, max_context |
| `src/provider/mod.rs` | chat, chat_stream, max_context |
| `src/daemon/session.rs` | chat, chat_stream, max_context |
| `src/agent/tests.rs` | chat, chat_stream, max_context |
| `tests/gateway_no_agent_map.rs` | no_agent_map_module_or_references_in_gateway, walk_rust_files |

## Entry Points

Start here when exploring this area:

- **`builtin_profile`** (Function) — `src/tools/profile.rs:187`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `builtin_profile` | Function | `src/tools/profile.rs` | 187 |
| `tool_names` | Function | `tests/contracts.rs` | 74 |
| `readonly_profile_does_not_expose_mutating_tools` | Function | `tests/contracts.rs` | 89 |
| `gateway_safe_profile_blocks_all_workspace_mutation` | Function | `tests/contracts.rs` | 111 |
| `default_registry_contains_foundational_tools` | Function | `tests/contracts.rs` | 136 |
| `gateway_safe_profile` | Function | `src/tools/profile.rs` | 142 |
| `gateway_safe_allows_no_workspace_mutation` | Function | `src/tools/profile.rs` | 258 |
| `vulcan_with_home` | Function | `tests/client_autostart.rs` | 20 |
| `wait_for_socket_gone` | Function | `tests/client_autostart.rs` | 27 |
| `cold_invocation_autostarts_daemon` | Function | `tests/client_autostart.rs` | 36 |
| `second_invocation_reuses_daemon` | Function | `tests/client_autostart.rs` | 56 |
| `autostart_handles_stale_socket` | Function | `tests/client_autostart.rs` | 87 |
| `autostart_race_settles_to_one_daemon` | Function | `tests/client_autostart.rs` | 105 |
| `ping_subcommand_hidden_from_help` | Function | `tests/client_autostart.rs` | 130 |
| `vulcan_with_home` | Function | `tests/daemon_e2e.rs` | 13 |
| `daemon_start_detach_status_stop` | Function | `tests/daemon_e2e.rs` | 33 |
| `daemon_status_fails_when_no_daemon` | Function | `tests/daemon_e2e.rs` | 76 |
| `daemon_socket_is_0600` | Function | `tests/daemon_e2e.rs` | 88 |
| `daemon_pid_file_is_0600` | Function | `tests/daemon_e2e.rs` | 114 |
| `chat` | Function | `tests/contracts.rs` | 28 |

## Connected Areas

| Area | Connections |
|------|-------------|
| Tools | 4 calls |
| Agent | 3 calls |

## How to Explore

1. `gitnexus_context({name: "builtin_profile"})` — see callers and callees
2. `gitnexus_query({query: "tests"})` — find related execution flows
3. Read key files listed above for implementation details
