---
name: config
description: "Skill for the Config area of vulcan. 131 symbols across 17 files."
---

# Config

131 symbols | 17 files | Cohesion: 72%

## When to Use

- Working with code in `src/`
- Understanding how bool_field, load_from, skills_dir work
- Modifying config-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/config/mod.rs` | load_from, vulcan_home, dirs_or_default, default, default_skills_dir (+45) |
| `src/config/tests.rs` | active_profile_unset_falls_back_to_legacy_provider, active_profile_set_resolves_to_named_provider, active_profile_pointing_at_missing_falls_back_to_legacy, provider_debug_mode_parses_from_toml, tools_profile_default_is_none (+44) |
| `src/cli_provider.rs` | interactive_use, use_profile, use_profile_writes_active_profile_when_target_exists, use_profile_rejects_unknown_target_before_writing, use_profile_clear_removes_active_profile (+1) |
| `src/cli_gateway.rs` | init, generate_api_token, init_writes_gateway_section_without_touching_active_profile, init_refuses_existing_gateway_without_force, init_force_replaces_existing_gateway |
| `src/cli_config.rs` | resolve_section_path, edit, paths |
| `src/playbook/mod.rs` | try_new, try_open_at, initialize |
| `src/code/embed.rs` | open, open_with_excluder, db_path_for |
| `src/extensions/config_field.rs` | bool_field, bool_field_round_trips_through_serde_json |
| `src/cli_model.rs` | write_legacy_provider_model, use_model_writes_to_legacy_provider_when_no_active_profile |
| `src/tui/keybinds.rs` | from_str |

## Entry Points

Start here when exploring this area:

- **`bool_field`** (Function) — `src/extensions/config_field.rs:34`
- **`load_from`** (Function) — `src/config/mod.rs:1414`
- **`skills_dir`** (Function) — `src/skills/mod.rs:214`
- **`try_new`** (Function) — `src/playbook/mod.rs:239`
- **`try_open_at`** (Function) — `src/playbook/mod.rs:246`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `bool_field` | Function | `src/extensions/config_field.rs` | 34 |
| `load_from` | Function | `src/config/mod.rs` | 1414 |
| `skills_dir` | Function | `src/skills/mod.rs` | 214 |
| `try_new` | Function | `src/playbook/mod.rs` | 239 |
| `try_open_at` | Function | `src/playbook/mod.rs` | 246 |
| `vulcan_home` | Function | `src/config/mod.rs` | 134 |
| `open` | Function | `src/code/embed.rs` | 60 |
| `open_with_excluder` | Function | `src/code/embed.rs` | 81 |
| `persist_active_profile_to_config` | Function | `src/agent/provider.rs` | 123 |
| `detect_unknown_top_level_keys` | Function | `src/config/mod.rs` | 1279 |
| `load_from_dir` | Function | `src/config/mod.rs` | 1344 |
| `init` | Function | `src/cli_gateway.rs` | 15 |
| `atomic_write` | Function | `src/config/mod.rs` | 13 |
| `validate` | Function | `src/config/mod.rs` | 478 |
| `snapshot_bak` | Function | `src/config/mod.rs` | 103 |
| `migrate` | Function | `src/config/mod.rs` | 1428 |
| `set_status` | Function | `src/extensions/registry.rs` | 112 |
| `apply_to_registry` | Function | `src/config/mod.rs` | 371 |
| `into_tool_profile` | Function | `src/config/mod.rs` | 1122 |
| `resolve_profile` | Function | `src/config/mod.rs` | 1136 |

## Execution Flows

| Flow | Type | Steps |
|------|------|-------|
| `Run_tui → Vulcan_home` | cross_community | 10 |
| `Webhook_loopback_accepts_signed_request → Dirs_or_default` | cross_community | 8 |
| `Post_inbound_enqueues_and_returns_id → Dirs_or_default` | cross_community | 8 |
| `Webhook_loopback_rejects_invalid_signature → Dirs_or_default` | cross_community | 8 |
| `Webhook_rejects_oversized_body_before_verification → Dirs_or_default` | cross_community | 8 |
| `Post_inbound_unknown_platform_returns_400 → Dirs_or_default` | cross_community | 8 |
| `Post_inbound_missing_bearer_returns_401 → Dirs_or_default` | cross_community | 8 |
| `Post_inbound_db_error_returns_503 → Dirs_or_default` | cross_community | 7 |
| `Get_scheduler_merges_run_history → Dirs_or_default` | cross_community | 7 |
| `Get_lanes_empty_cache_returns_empty_array → Dirs_or_default` | cross_community | 7 |

## Connected Areas

| Area | Connections |
|------|-------------|
| Cluster_33 | 4 calls |
| Code | 3 calls |
| Extensions | 2 calls |
| State | 1 calls |
| Tools | 1 calls |
| Tests | 1 calls |
| Routes | 1 calls |

## How to Explore

1. `gitnexus_context({name: "bool_field"})` — see callers and callees
2. `gitnexus_query({query: "config"})` — find related execution flows
3. Read key files listed above for implementation details
