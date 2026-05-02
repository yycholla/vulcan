---
name: agent
description: "Skill for the Agent area of vulcan. 107 symbols across 17 files."
---

# Agent

107 symbols | 17 files | Cohesion: 73%

## When to Use

- Working with code in `src/`
- Understanding how channel, with_pause, enqueue work
- Modifying agent-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/agent/tests.rs` | empty_skills, agent_with_mock, single_turn_text_response, multi_turn_with_tool_call, streaming_and_buffered_paths_match (+21) |
| `src/agent/run.rs` | run_prompt_stream, run_prompt_stream_with_cancel, compact_stream_messages_if_needed, empty_terminal_message, record_run_event (+14) |
| `src/agent/mod.rs` | session_id, run_store, artifact_store, for_test, with_hooks (+8) |
| `tests/agent_loop.rs` | agent_with_mock, run_record_lifecycle_events_land_for_completed_turn, agent_create_artifact_persists_with_run_and_session_links, run_record_gateway_origin_carries_lane_string, run_record_captures_streaming_turn_with_tui_origin (+5) |
| `src/context.rs` | with_config, compaction_can_be_disabled_by_config, trigger_ratio_comes_from_config, reserved_tokens_come_from_config_and_still_scale_to_context, summarization_request (+2) |
| `src/provider/mock.rs` | enqueue, enqueue_text, enqueue_tool_call, enqueue_tool_calls, enqueue_error (+1) |
| `src/agent/skills.rs` | auto_create_skill_from_turn, strip_json_fence, sanitize_skill_name, render_skill_markdown, sanitize_skill_name_caps_length (+1) |
| `tests/contracts.rs` | agent_with_profile, disallowed_tool_call_produces_structured_denial_in_run_record, tool_errors_are_distinguishable_from_successes, happy_turn_produces_no_provider_error_events |
| `src/tools/file.rs` | with_pause, patch_file_with_pause_routes_through_scrubber_and_applies_subset, patch_file_with_pause_reject_all_leaves_file_unchanged |
| `src/agent/dispatch.rs` | summarize_tool_args, summarize_tool_result, preview_output |

## Entry Points

Start here when exploring this area:

- **`channel`** (Function) — `src/pause.rs:133`
- **`with_pause`** (Function) — `src/tools/file.rs:685`
- **`enqueue`** (Function) — `src/provider/mock.rs:62`
- **`enqueue_text`** (Function) — `src/provider/mock.rs:67`
- **`enqueue_tool_call`** (Function) — `src/provider/mock.rs:71`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `channel` | Function | `src/pause.rs` | 133 |
| `with_pause` | Function | `src/tools/file.rs` | 685 |
| `enqueue` | Function | `src/provider/mock.rs` | 62 |
| `enqueue_text` | Function | `src/provider/mock.rs` | 67 |
| `enqueue_tool_call` | Function | `src/provider/mock.rs` | 71 |
| `enqueue_tool_calls` | Function | `src/provider/mock.rs` | 90 |
| `enqueue_error` | Function | `src/provider/mock.rs` | 105 |
| `captured_calls` | Function | `src/provider/mock.rs` | 110 |
| `fork_session` | Function | `src/agent/session.rs` | 50 |
| `memory` | Function | `src/agent/session.rs` | 69 |
| `run_prompt_stream` | Function | `src/agent/run.rs` | 364 |
| `run_prompt_stream_with_cancel` | Function | `src/agent/run.rs` | 404 |
| `compact_stream_messages_if_needed` | Function | `src/agent/run.rs` | 678 |
| `session_id` | Function | `src/agent/mod.rs` | 600 |
| `run_store` | Function | `src/agent/mod.rs` | 642 |
| `artifact_store` | Function | `src/agent/mod.rs` | 656 |
| `for_test` | Function | `src/agent/mod.rs` | 709 |
| `run_review` | Function | `src/review/runner.rs` | 32 |
| `with_hooks` | Function | `src/agent/mod.rs` | 193 |
| `with_pause_channel` | Function | `src/agent/mod.rs` | 198 |

## Execution Flows

| Flow | Type | Steps |
|------|------|-------|
| `Main → Name` | cross_community | 9 |
| `Main → StoreInner` | cross_community | 9 |
| `Parallel_tool_calls_dispatch_concurrently → Name` | cross_community | 9 |
| `Parallel_tool_calls_dispatch_concurrently → StoreInner` | cross_community | 9 |
| `Agent_create_artifact_persists_with_run_and_session_links → Name` | cross_community | 9 |
| `Agent_create_artifact_persists_with_run_and_session_links → StoreInner` | cross_community | 7 |
| `Agent_create_artifact_persists_with_run_and_session_links → KeyParseError` | cross_community | 7 |
| `Agent_create_artifact_persists_with_run_and_session_links → Next` | cross_community | 7 |
| `Main → KeyParseError` | cross_community | 6 |
| `Main → Next` | cross_community | 6 |

## Connected Areas

| Area | Connections |
|------|-------------|
| Tools | 7 calls |
| Tui | 5 calls |
| Impact | 5 calls |
| Provider | 5 calls |
| Routes | 3 calls |
| Artifact | 3 calls |
| Memory | 3 calls |
| Config | 2 calls |

## How to Explore

1. `gitnexus_context({name: "channel"})` — see callers and callees
2. `gitnexus_query({query: "agent"})` — find related execution flows
3. Read key files listed above for implementation details
