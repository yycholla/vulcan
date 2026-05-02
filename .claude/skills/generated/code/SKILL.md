---
name: code
description: "Skill for the Code area of vulcan. 34 symbols across 11 files."
---

# Code

34 symbols | 11 files | Cohesion: 68%

## When to Use

- Working with code in `src/`
- Understanding how match_native_category, from_path, name work
- Modifying code-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/code/mod.rs` | from_path, name, grammar, outline_query, new (+2) |
| `src/code/embed.rs` | endpoint, api_key, embed, reindex, search (+2) |
| `src/code/graph.rs` | open, reindex, find_by_name, extract_symbols, reindex_and_find_symbol_round_trip (+1) |
| `src/tools/code.rs` | call, call, outline, run_query, extract_returns_just_the_named_function |
| `src/tools/code_graph.rs` | call, call |
| `src/gateway/discord.rs` | map_attachment, map_attachment_classifies_kind_from_mime |
| `src/tools/code_search.rs` | call |
| `src/impact/generator.rs` | extract_symbols |
| `src/hooks/prefer_native.rs` | match_native_category |
| `src/hooks/audit.rs` | record_bash |

## Entry Points

Start here when exploring this area:

- **`match_native_category`** (Function) — `src/hooks/prefer_native.rs:86`
- **`from_path`** (Function) — `src/code/mod.rs:30`
- **`name`** (Function) — `src/code/mod.rs:46`
- **`outline_query`** (Function) — `src/code/mod.rs:88`
- **`new`** (Function) — `src/code/mod.rs:149`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `match_native_category` | Function | `src/hooks/prefer_native.rs` | 86 |
| `from_path` | Function | `src/code/mod.rs` | 30 |
| `name` | Function | `src/code/mod.rs` | 46 |
| `outline_query` | Function | `src/code/mod.rs` | 88 |
| `new` | Function | `src/code/mod.rs` | 149 |
| `with_parser` | Function | `src/code/mod.rs` | 162 |
| `open` | Function | `src/code/graph.rs` | 40 |
| `reindex` | Function | `src/code/graph.rs` | 91 |
| `find_by_name` | Function | `src/code/graph.rs` | 146 |
| `embed` | Function | `src/code/embed.rs` | 142 |
| `reindex` | Function | `src/code/embed.rs` | 192 |
| `search` | Function | `src/code/embed.rs` | 278 |
| `next` | Function | `src/tui/state/mod.rs` | 87 |
| `call` | Function | `src/tools/code_search.rs` | 47 |
| `call` | Function | `src/tools/code_graph.rs` | 47 |
| `call` | Function | `src/tools/code_graph.rs` | 92 |
| `call` | Function | `src/tools/code.rs` | 127 |
| `call` | Function | `src/tools/code.rs` | 192 |
| `outline` | Function | `src/tools/code.rs` | 223 |
| `run_query` | Function | `src/tools/code.rs` | 284 |

## Execution Flows

| Flow | Type | Steps |
|------|------|-------|
| `Run_tui → Vulcan_home` | cross_community | 10 |
| `Search → Dirs_or_default` | cross_community | 7 |
| `Agent_create_artifact_persists_with_run_and_session_links → Next` | cross_community | 7 |
| `Main → Next` | cross_community | 6 |
| `Search → Write_frame_bytes` | cross_community | 6 |
| `Parallel_tool_calls_dispatch_concurrently → Next` | cross_community | 6 |
| `Run → New` | cross_community | 5 |
| `Run → Grammar` | cross_community | 5 |
| `Run → Next` | cross_community | 5 |
| `Run → Next` | cross_community | 5 |

## Connected Areas

| Area | Connections |
|------|-------------|
| Tools | 5 calls |
| Tui | 3 calls |
| Impact | 2 calls |
| Config | 1 calls |
| Daemon | 1 calls |

## How to Explore

1. `gitnexus_context({name: "match_native_category"})` — see callers and callees
2. `gitnexus_query({query: "code"})` — find related execution flows
3. Read key files listed above for implementation details
