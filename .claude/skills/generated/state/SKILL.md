---
name: state
description: "Skill for the State area of vulcan. 98 symbols across 13 files."
---

# State

98 symbols | 13 files | Cohesion: 79%

## When to Use

- Working with code in `src/`
- Understanding how frame, section_header, reasoning_lines work
- Modifying state-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/tui/state/mod.rs` | badge, tool_log_view, mode_label, prompt_hints, model_status (+28) |
| `src/tui/state/tests.rs` | model_status_omits_prefix_when_no_provider_label, model_status_prefixes_active_provider_label, prompt_hints_default_keybinds_match_ascii_labels, prompt_hints_returns_borrowed_slice_no_alloc, estimated_cost_multiplies_tokens_by_per_token_pricing (+13) |
| `src/cli_auth.rs` | run, run_preset, run_custom, profile_exists_prompt, prompt_api_key (+7) |
| `src/tui/views.rs` | render_view, title, publish_chat_max_scroll, single_stack, split_sessions (+6) |
| `src/tui/widgets.rs` | frame, section_header, reasoning_lines, prompt_row_height, prompt_row (+2) |
| `src/cli_provider.rs` | presets, lookup_preset, interactive_add, presets_catalog_has_expected_minimum |
| `src/tui/chat_message.rs` | set_content, render_version, push_tool_start, finish_tool |
| `src/tui/events.rs` | submit_prompt, handle_stream_event, refresh_sessions |
| `src/tui/theme.rs` | body, faint_bg |
| `src/cli_playbook.rs` | shorten |

## Entry Points

Start here when exploring this area:

- **`frame`** (Function) — `src/tui/widgets.rs:20`
- **`section_header`** (Function) — `src/tui/widgets.rs:76`
- **`reasoning_lines`** (Function) — `src/tui/widgets.rs:168`
- **`prompt_row_height`** (Function) — `src/tui/widgets.rs:288`
- **`prompt_row`** (Function) — `src/tui/widgets.rs:300`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `frame` | Function | `src/tui/widgets.rs` | 20 |
| `section_header` | Function | `src/tui/widgets.rs` | 76 |
| `reasoning_lines` | Function | `src/tui/widgets.rs` | 168 |
| `prompt_row_height` | Function | `src/tui/widgets.rs` | 288 |
| `prompt_row` | Function | `src/tui/widgets.rs` | 300 |
| `ticker` | Function | `src/tui/widgets.rs` | 408 |
| `fill` | Function | `src/tui/widgets.rs` | 642 |
| `render_view` | Function | `src/tui/views.rs` | 16 |
| `title` | Function | `src/tui/views.rs` | 37 |
| `body` | Function | `src/tui/theme.rs` | 23 |
| `faint_bg` | Function | `src/tui/theme.rs` | 40 |
| `draw_palette` | Function | `src/tui/rendering.rs` | 28 |
| `count` | Function | `src/code/graph.rs` | 168 |
| `badge` | Function | `src/tui/state/mod.rs` | 124 |
| `tool_log_view` | Function | `src/tui/state/mod.rs` | 436 |
| `mode_label` | Function | `src/tui/state/mod.rs` | 465 |
| `prompt_hints` | Function | `src/tui/state/mod.rs` | 474 |
| `model_status` | Function | `src/tui/state/mod.rs` | 506 |
| `estimated_cost` | Function | `src/tui/state/mod.rs` | 535 |
| `context_ratio` | Function | `src/tui/state/mod.rs` | 560 |

## Execution Flows

| Flow | Type | Steps |
|------|------|-------|
| `Run → Parse_ipv4` | intra_community | 5 |
| `Run → Next` | cross_community | 5 |
| `Run → For_base_url` | cross_community | 5 |
| `Run → Preset` | cross_community | 5 |
| `Run → Read_or_init_doc` | cross_community | 5 |
| `Trading_floor → Count` | intra_community | 5 |
| `Chat → Count` | cross_community | 5 |
| `Handle_stream_event → OrchestrationEvent` | intra_community | 4 |
| `Run → With_theme` | cross_community | 4 |
| `Run → With_theme` | intra_community | 4 |

## Connected Areas

| Area | Connections |
|------|-------------|
| Tui | 17 calls |
| Cluster_33 | 3 calls |
| Orchestration | 3 calls |
| Agent | 2 calls |
| Code | 1 calls |
| Impact | 1 calls |
| Provider | 1 calls |
| Config | 1 calls |

## How to Explore

1. `gitnexus_context({name: "frame"})` — see callers and callees
2. `gitnexus_query({query: "state"})` — find related execution flows
3. Read key files listed above for implementation details
