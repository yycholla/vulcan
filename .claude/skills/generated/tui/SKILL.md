---
name: tui
description: "Skill for the Tui area of vulcan. 138 symbols across 25 files."
---

# Tui

138 symbols | 25 files | Cohesion: 76%

## When to Use

- Working with code in `src/`
- Understanding how system, visible_lines, visible_lines_at work
- Modifying tui-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/tui/chat_render.rs` | visible_lines, visible_lines_at, render_message_block, render_count, render_count_for_tests (+20) |
| `src/tui/model_picker.rs` | build_model_tree, split_lab, build_lab_node, build_series_node, tokenize (+12) |
| `src/tui/theme.rs` | system, system_assistant_inherits_terminal_fg, from_name, from_name_system_returns_reset_bg, all_themes_inherit_terminal_bg (+6) |
| `src/tui/miller_columns.rs` | new, move_cursor, drill, render, draw_column (+5) |
| `src/tui/chat_message.rs` | bump_render_version, append_text, append_reasoning, push_tool_start_with, finish_tool_with (+2) |
| `src/tui/rendering.rs` | draw_session_picker, draw_provider_picker, draw_diff_scrubber, draw_picker_border, draw_model_picker (+2) |
| `src/tui/state/mod.rs` | new, demo_diff, cursor, format_thousands, build (+1) |
| `benches/tui_render.rs` | out_path, synthetic_transcript, visible_lines_first_render, visible_lines_cached_tail, main |
| `src/tui/keymap.rs` | filter_commands, current_palette, complete_slash, format_model_list, build_provider_picker_entries |
| `src/tui/keybinds.rs` | from_config, defaults, default, keybinds_from_config_uses_defaults_for_unparseable, keybinds_from_config_parses_overrides |

## Entry Points

Start here when exploring this area:

- **`system`** (Function) — `src/tui/theme.rs:102`
- **`visible_lines`** (Function) — `src/tui/chat_render.rs:68`
- **`visible_lines_at`** (Function) — `src/tui/chat_render.rs:87`
- **`render_count`** (Function) — `src/tui/chat_render.rs:281`
- **`render_count_for_tests`** (Function) — `src/tui/chat_render.rs:290`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `system` | Function | `src/tui/theme.rs` | 102 |
| `visible_lines` | Function | `src/tui/chat_render.rs` | 68 |
| `visible_lines_at` | Function | `src/tui/chat_render.rs` | 87 |
| `render_count` | Function | `src/tui/chat_render.rs` | 281 |
| `render_count_for_tests` | Function | `src/tui/chat_render.rs` | 290 |
| `append_text` | Function | `src/tui/chat_message.rs` | 119 |
| `append_reasoning` | Function | `src/tui/chat_message.rs` | 131 |
| `push_tool_start_with` | Function | `src/tui/chat_message.rs` | 147 |
| `finish_tool_with` | Function | `src/tui/chat_message.rs` | 173 |
| `new` | Function | `src/tui/state/mod.rs` | 347 |
| `draw_session_picker` | Function | `src/tui/rendering.rs` | 90 |
| `draw_provider_picker` | Function | `src/tui/rendering.rs` | 405 |
| `draw_diff_scrubber` | Function | `src/tui/rendering.rs` | 485 |
| `run_tui` | Function | `src/tui/mod.rs` | 90 |
| `filter_commands` | Function | `src/tui/keymap.rs` | 190 |
| `current_palette` | Function | `src/tui/keymap.rs` | 204 |
| `complete_slash` | Function | `src/tui/keymap.rs` | 214 |
| `init_terminal` | Function | `src/tui/init.rs` | 11 |
| `restore_terminal` | Function | `src/tui/init.rs` | 34 |
| `effective_stream_channel_capacity` | Function | `src/config/mod.rs` | 975 |

## Execution Flows

| Flow | Type | Steps |
|------|------|-------|
| `Run_tui → StoreInner` | cross_community | 10 |
| `Run_tui → Vulcan_home` | cross_community | 10 |
| `Run_tui → Name` | cross_community | 9 |
| `Main → StoreInner` | cross_community | 9 |
| `Parallel_tool_calls_dispatch_concurrently → StoreInner` | cross_community | 9 |
| `Agent_create_artifact_persists_with_run_and_session_links → StoreInner` | cross_community | 7 |
| `Run → StoreInner` | cross_community | 5 |
| `Webhook_loopback_accepts_signed_request → Build` | cross_community | 4 |
| `Handle → KeyParseError` | cross_community | 4 |
| `Handle → Next` | cross_community | 4 |

## Connected Areas

| Area | Connections |
|------|-------------|
| State | 12 calls |
| Agent | 8 calls |
| Hooks | 4 calls |
| Routes | 3 calls |
| Cluster_269 | 1 calls |
| Impact | 1 calls |
| Memory | 1 calls |
| Orchestration | 1 calls |

## How to Explore

1. `gitnexus_context({name: "system"})` — see callers and callees
2. `gitnexus_query({query: "tui"})` — find related execution flows
3. Read key files listed above for implementation details
