---
name: provider
description: "Skill for the Provider area of vulcan. 132 symbols across 9 files."
---

# Provider

132 symbols | 9 files | Cohesion: 80%

## When to Use

- Working with code in `src/`
- Understanding how flush, logs_wire, logs_tool_fallback work
- Modifying provider-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/provider/openai.rs` | parse_line, log_tool_fallback_if_enabled, summarize_wire_response, log_wire_response, chat (+49) |
| `src/provider/redact.rs` | secret_patterns, redact_string, redact_response_text, redact_string_strips_bearer_token, redact_string_strips_bare_sk_key (+17) |
| `src/provider/think_sanitizer.rs` | flush, new, feed, find_tag_ci, whole_block_in_one_chunk_routes_to_reasoning (+12) |
| `src/provider/catalog.rs` | list_models, parse_openrouter, list_models, cache_path, host_slug (+10) |
| `src/provider/mock.rs` | new, new, chat, chat_stream, generated_provider_calls_script_with_turn_index (+4) |
| `src/provider/factory.rs` | build, cfg_with_type, default_factory_builds_openai_for_default_type, default_factory_accepts_empty_type_alias, default_factory_accepts_openai_alias (+1) |
| `src/provider/mod.rs` | from_response, extract_error_message, is_retryable, normalize_base_url |
| `src/agent/provider.rs` | available_models, fetch_catalog_for, resolve_model_selection |
| `src/config/mod.rs` | logs_wire, logs_tool_fallback |

## Entry Points

Start here when exploring this area:

- **`flush`** (Function) — `src/provider/think_sanitizer.rs:119`
- **`logs_wire`** (Function) — `src/config/mod.rs:890`
- **`logs_tool_fallback`** (Function) — `src/config/mod.rs:894`
- **`redact_string`** (Function) — `src/provider/redact.rs:139`
- **`redact_response_text`** (Function) — `src/provider/redact.rs:173`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `flush` | Function | `src/provider/think_sanitizer.rs` | 119 |
| `logs_wire` | Function | `src/config/mod.rs` | 890 |
| `logs_tool_fallback` | Function | `src/config/mod.rs` | 894 |
| `redact_string` | Function | `src/provider/redact.rs` | 139 |
| `redact_response_text` | Function | `src/provider/redact.rs` | 173 |
| `new` | Function | `src/provider/think_sanitizer.rs` | 43 |
| `feed` | Function | `src/provider/think_sanitizer.rs` | 50 |
| `from_response` | Function | `src/provider/mod.rs` | 123 |
| `is_retryable` | Function | `src/provider/mod.rs` | 111 |
| `new` | Function | `src/provider/mock.rs` | 54 |
| `new` | Function | `src/provider/mock.rs` | 230 |
| `for_base_url` | Function | `src/provider/catalog.rs` | 297 |
| `fuzzy_suggest` | Function | `src/provider/catalog.rs` | 368 |
| `available_models` | Function | `src/agent/provider.rs` | 14 |
| `fetch_catalog_for` | Function | `src/agent/provider.rs` | 182 |
| `resolve_model_selection` | Function | `src/agent/provider.rs` | 196 |
| `redact_value` | Function | `src/provider/redact.rs` | 155 |
| `new` | Function | `src/provider/openai.rs` | 36 |
| `normalize_base_url` | Function | `src/provider/mod.rs` | 448 |
| `new` | Function | `src/provider/catalog.rs` | 69 |

## Execution Flows

| Flow | Type | Steps |
|------|------|-------|
| `Open_unified_picker → For_base_url` | cross_community | 6 |
| `Ensure_agent → For_base_url` | cross_community | 6 |
| `Run → For_base_url` | cross_community | 5 |
| `Open_unified_picker → Fuzzy_suggest` | cross_community | 5 |
| `Open_unified_picker → ModelSelection` | cross_community | 5 |
| `Chat → Count` | cross_community | 5 |
| `Chat → Secret_patterns` | cross_community | 5 |
| `Ensure_agent → Fuzzy_suggest` | cross_community | 5 |
| `Ensure_agent → ModelSelection` | cross_community | 5 |
| `Chat_stream → StoreInner` | cross_community | 4 |

## Connected Areas

| Area | Connections |
|------|-------------|
| Tui | 4 calls |
| Config | 4 calls |
| State | 3 calls |
| Daemon | 2 calls |
| Impact | 1 calls |

## How to Explore

1. `gitnexus_context({name: "flush"})` — see callers and callees
2. `gitnexus_query({query: "provider"})` — find related execution flows
3. Read key files listed above for implementation details
