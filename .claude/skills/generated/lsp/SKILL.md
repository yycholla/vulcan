---
name: lsp
description: "Skill for the Lsp area of vulcan. 42 symbols across 6 files."
---

# Lsp

42 symbols | 6 files | Cohesion: 77%

## When to Use

- Working with code in `src/`
- Understanding how goto_definition, find_references, hover work
- Modifying lsp-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/code/lsp/mod.rs` | is_indexing, handshake, next_id, request, wait_until_ready (+19) |
| `src/code/lsp/requests.rs` | prepare_request, goto_definition, find_references, hover, type_definition (+7) |
| `src/hooks/diagnostics.rs` | after_tool_call, severity_rank, severity_label |
| `src/tools/lsp.rs` | call |
| `src/tools/code_edit.rs` | call |
| `src/code/mod.rs` | default |

## Entry Points

Start here when exploring this area:

- **`goto_definition`** (Function) — `src/code/lsp/requests.rs:34`
- **`find_references`** (Function) — `src/code/lsp/requests.rs:56`
- **`hover`** (Function) — `src/code/lsp/requests.rs:80`
- **`type_definition`** (Function) — `src/code/lsp/requests.rs:103`
- **`implementation`** (Function) — `src/code/lsp/requests.rs:128`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `goto_definition` | Function | `src/code/lsp/requests.rs` | 34 |
| `find_references` | Function | `src/code/lsp/requests.rs` | 56 |
| `hover` | Function | `src/code/lsp/requests.rs` | 80 |
| `type_definition` | Function | `src/code/lsp/requests.rs` | 103 |
| `implementation` | Function | `src/code/lsp/requests.rs` | 128 |
| `prepare_call_hierarchy` | Function | `src/code/lsp/requests.rs` | 172 |
| `call_hierarchy_incoming` | Function | `src/code/lsp/requests.rs` | 196 |
| `call_hierarchy_outgoing` | Function | `src/code/lsp/requests.rs` | 214 |
| `workspace_symbol` | Function | `src/code/lsp/requests.rs` | 236 |
| `code_action` | Function | `src/code/lsp/requests.rs` | 262 |
| `diagnostics_for` | Function | `src/code/lsp/requests.rs` | 292 |
| `request` | Function | `src/code/lsp/mod.rs` | 429 |
| `wait_until_ready` | Function | `src/code/lsp/mod.rs` | 481 |
| `mark_ready` | Function | `src/code/lsp/mod.rs` | 510 |
| `notify` | Function | `src/code/lsp/mod.rs` | 517 |
| `did_open` | Function | `src/code/lsp/mod.rs` | 531 |
| `cached_diagnostics` | Function | `src/code/lsp/mod.rs` | 558 |
| `path_to_uri` | Function | `src/code/lsp/mod.rs` | 630 |
| `spawn_no_handshake` | Function | `src/code/lsp/mod.rs` | 347 |
| `is_alive` | Function | `src/code/lsp/mod.rs` | 502 |

## Connected Areas

| Area | Connections |
|------|-------------|
| Code | 7 calls |
| Tools | 5 calls |
| Config | 1 calls |

## How to Explore

1. `gitnexus_context({name: "goto_definition"})` — see callers and callees
2. `gitnexus_query({query: "lsp"})` — find related execution flows
3. Read key files listed above for implementation details
