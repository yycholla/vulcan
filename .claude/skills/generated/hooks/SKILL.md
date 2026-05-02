---
name: hooks
description: "Skill for the Hooks area of vulcan. 148 symbols across 15 files."
---

# Hooks

148 symbols | 15 files | Cohesion: 89%

## When to Use

- Working with code in `src/`
- Understanding how new, with_pause_emitter, with_config work
- Modifying hooks-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/hooks/safety.rs` | with_cap, new, with_pause_emitter, with_config, approve (+65) |
| `src/hooks/mod.rs` | snapshot, new, with_timeout, register, failure_metrics (+13) |
| `src/hooks/audit.rs` | new, with_bash_counters, before_tool_call, after_tool_call, first_line (+8) |
| `src/hooks/recall.rs` | new, before_prompt, sanitize_fts_query, is_fresh_start, user (+6) |
| `src/hooks/skills.rs` | matched_skills, activation_tokens, latest_user_message, before_prompt, skill (+4) |
| `src/hooks/prefer_native.rs` | new, before_tool_call, match_native_redirect, hook_block_mode_blocks_bash_redirect, hook_warn_mode_passes_through (+2) |
| `src/hooks/cortex_capture.rs` | new, new, content_hash, summarize, after_tool_call |
| `src/hooks/approval.rs` | new, auto_deny, before_tool_call, approval_hook_auto_denies_when_no_pause_channel, approval_hook_auto_deny_still_allows_always_mode |
| `src/tui/mod.rs` | format_provider_list_marks_active_profile_and_lists_named, format_provider_list_handles_no_named_profiles |
| `src/memory/tests.rs` | queue_tables_created, queue_indexes_created |

## Entry Points

Start here when exploring this area:

- **`new`** (Function) — `src/hooks/safety.rs:154`
- **`with_pause_emitter`** (Function) — `src/hooks/safety.rs:161`
- **`with_config`** (Function) — `src/hooks/safety.rs:167`
- **`approve`** (Function) — `src/hooks/safety.rs:208`
- **`format_provider_list`** (Function) — `src/tui/keymap.rs:161`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `new` | Function | `src/hooks/safety.rs` | 154 |
| `with_pause_emitter` | Function | `src/hooks/safety.rs` | 161 |
| `with_config` | Function | `src/hooks/safety.rs` | 167 |
| `approve` | Function | `src/hooks/safety.rs` | 208 |
| `format_provider_list` | Function | `src/tui/keymap.rs` | 161 |
| `new` | Function | `src/hooks/mod.rs` | 165 |
| `with_timeout` | Function | `src/hooks/mod.rs` | 173 |
| `register` | Function | `src/hooks/mod.rs` | 178 |
| `failure_metrics` | Function | `src/hooks/mod.rs` | 191 |
| `apply_before_prompt` | Function | `src/hooks/mod.rs` | 199 |
| `before_tool_call` | Function | `src/hooks/mod.rs` | 255 |
| `after_tool_call` | Function | `src/hooks/mod.rs` | 288 |
| `before_agent_end` | Function | `src/hooks/mod.rs` | 318 |
| `in_memory` | Function | `src/memory/mod.rs` | 417 |
| `new` | Function | `src/hooks/recall.rs` | 28 |
| `new` | Function | `src/hooks/audit.rs` | 96 |
| `with_bash_counters` | Function | `src/hooks/audit.rs` | 104 |
| `new` | Function | `src/hooks/prefer_native.rs` | 31 |
| `match_native_redirect` | Function | `src/hooks/prefer_native.rs` | 129 |
| `fact` | Function | `src/memory/cortex.rs` | 76 |

## Execution Flows

| Flow | Type | Steps |
|------|------|-------|
| `Run_tui → StoreInner` | cross_community | 10 |
| `Run_tui → Vulcan_home` | cross_community | 10 |
| `Run_tui → Name` | cross_community | 9 |
| `Recall_hook_injects_when_fts_returns_hits → Apply_connection_pragmas` | cross_community | 7 |
| `Agent_create_artifact_persists_with_run_and_session_links → Apply_connection_pragmas` | cross_community | 6 |
| `Recall_hook_injects_when_fts_returns_hits → Dirs_or_default` | cross_community | 6 |
| `Main → Apply_connection_pragmas` | cross_community | 5 |
| `Parallel_tool_calls_dispatch_concurrently → Apply_connection_pragmas` | cross_community | 5 |
| `Run → Fact` | cross_community | 4 |
| `Run_tui → Default` | cross_community | 3 |

## Connected Areas

| Area | Connections |
|------|-------------|
| Code | 4 calls |
| Tools | 2 calls |
| Memory | 2 calls |
| Agent | 2 calls |
| State | 2 calls |
| Tui | 2 calls |
| Skills | 1 calls |
| Client | 1 calls |

## How to Explore

1. `gitnexus_context({name: "new"})` — see callers and callees
2. `gitnexus_query({query: "hooks"})` — find related execution flows
3. Read key files listed above for implementation details
