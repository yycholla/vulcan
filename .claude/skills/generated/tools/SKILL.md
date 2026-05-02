---
name: tools
description: "Skill for the Tools area of vulcan. 312 symbols across 23 files."
---

# Tools

312 symbols | 23 files | Cohesion: 67%

## When to Use

- Working with code in `src/`
- Understanding how parse_tool_params, as_str, create_artifact work
- Modifying tools-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/tools/mod.rs` | parse_tool_params, as_str, missing_required_fields, validate_tool_params, yyc263_parse_tool_params_returns_typed_struct_on_valid_input (+52) |
| `src/tools/shell.rs` | call, call, call, make_tools, new (+45) |
| `src/tools/file.rs` | call, write_file_refuses_oversized_content, write_file_accepts_at_cap_content, yyc264_concurrent_write_file_to_same_path_lands_deterministically, write_file_uses_atomic_rename_so_no_partial_file_visible (+44) |
| `src/tools/web.rs` | call, shared_client, call, yyc256_shared_client_returns_same_pointer_each_call, yyc263_web_search_missing_query_surfaces_as_toolresult_err (+25) |
| `src/tools/fs_sandbox.rs` | validate_read, blocks_read_of_etc_shadow, blocks_read_of_proc_self_environ, blocks_read_of_sys, blocks_read_of_dev_block_devices (+17) |
| `src/tools/web_ssrf.rs` | validate, rejects_imds_endpoint, rejects_ipv4_loopback, rejects_ipv6_loopback, rejects_rfc1918_private_ranges (+13) |
| `src/tools/lsp.rs` | lang_for, call, call, call, call (+8) |
| `src/tools/git.rs` | run_git, call, call, call, call (+7) |
| `src/cli_model.rs` | run, interactive_pick, interactive_pick_id, show, list (+5) |
| `src/tools/spawn.rs` | with_artifact_store, default_allowed_tools, new, with_store, call (+5) |

## Entry Points

Start here when exploring this area:

- **`parse_tool_params`** (Function) — `src/tools/mod.rs:109`
- **`as_str`** (Function) — `src/tools/mod.rs:144`
- **`create_artifact`** (Function) — `src/agent/mod.rs:681`
- **`run`** (Function) — `src/cli_model.rs:12`
- **`fetch_catalog`** (Function) — `src/cli_model.rs:196`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `parse_tool_params` | Function | `src/tools/mod.rs` | 109 |
| `as_str` | Function | `src/tools/mod.rs` | 144 |
| `create_artifact` | Function | `src/agent/mod.rs` | 681 |
| `run` | Function | `src/cli_model.rs` | 12 |
| `fetch_catalog` | Function | `src/cli_model.rs` | 196 |
| `with_artifact_store` | Function | `src/tools/spawn.rs` | 126 |
| `filter_for_context` | Function | `src/tools/mod.rs` | 564 |
| `active_provider_config` | Function | `src/config/mod.rs` | 316 |
| `api_key` | Function | `src/config/mod.rs` | 1549 |
| `max_context` | Function | `src/agent/mod.rs` | 596 |
| `build_system_prompt` | Function | `src/prompt_builder.rs` | 6 |
| `build_system_prompt_with_context` | Function | `src/prompt_builder.rs` | 10 |
| `ok` | Function | `src/tools/mod.rs` | 36 |
| `err` | Function | `src/tools/mod.rs` | 46 |
| `new` | Function | `src/tools/mod.rs` | 431 |
| `new_with_diff_sink` | Function | `src/tools/mod.rs` | 444 |
| `new_with_diff_and_lsp` | Function | `src/tools/mod.rs` | 454 |
| `validate` | Function | `src/tools/web_ssrf.rs` | 44 |
| `new` | Function | `src/tools/spawn.rs` | 109 |
| `with_store` | Function | `src/tools/spawn.rs` | 116 |

## Execution Flows

| Flow | Type | Steps |
|------|------|-------|
| `Run_tui → StoreInner` | cross_community | 10 |
| `Run_tui → Vulcan_home` | cross_community | 10 |
| `Run_tui → Name` | cross_community | 9 |
| `Main → Name` | cross_community | 9 |
| `Main → StoreInner` | cross_community | 9 |
| `Parallel_tool_calls_dispatch_concurrently → Name` | cross_community | 9 |
| `Parallel_tool_calls_dispatch_concurrently → StoreInner` | cross_community | 9 |
| `Agent_create_artifact_persists_with_run_and_session_links → Name` | cross_community | 9 |
| `Agent_create_artifact_persists_with_run_and_session_links → StoreInner` | cross_community | 7 |
| `Agent_create_artifact_persists_with_run_and_session_links → KeyParseError` | cross_community | 7 |

## Connected Areas

| Area | Connections |
|------|-------------|
| Config | 13 calls |
| Lsp | 10 calls |
| Tui | 9 calls |
| Code | 8 calls |
| Agent | 8 calls |
| Impact | 5 calls |
| Hooks | 5 calls |
| Artifact | 3 calls |

## How to Explore

1. `gitnexus_context({name: "parse_tool_params"})` — see callers and callees
2. `gitnexus_query({query: "tools"})` — find related execution flows
3. Read key files listed above for implementation details
