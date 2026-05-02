---
name: extensions
description: "Skill for the Extensions area of vulcan. 130 symbols across 12 files."
---

# Extensions

130 symbols | 12 files | Cohesion: 72%

## When to Use

- Working with code in `src/`
- Understanding how run, from_toml_str, new work
- Modifying extensions-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/extensions/registry.rs` | get, load_from_store, load_from_store_with_version, write_manifest, load_from_store_imports_manifests_with_inactive_default (+27) |
| `src/extensions/install_state.rs` | row_to_state, upsert, get, list, set_enabled (+12) |
| `src/extensions/audit.rs` | default, new, new, record, reset (+10) |
| `src/extensions/verify.rs` | verify_checksum_optional, manifest_with, malformed_versions_pass_leniently, checksum_no_op_when_field_absent, checksum_no_op_when_payload_absent (+8) |
| `src/extensions/policy.rs` | is_sensitive, new, set_override, decide, declared (+7) |
| `src/extensions/manifest.rs` | from_toml_str, validate, valid_id, parses_minimal_builtin_manifest, parses_local_script_with_capabilities_and_permissions (+6) |
| `src/cli_extension.rs` | run, show, set_enabled, uninstall, scaffold_new (+5) |
| `src/extensions/draft.rs` | parse_skill_extension, extract_frontmatter, extract_indented_block, strip_quotes, parse_capability_list (+4) |
| `src/extensions/store.rs` | discover, empty_store_returns_empty_vec, missing_extensions_dir_returns_empty_vec, discovers_valid_manifest, surfaces_parse_error_without_breaking_other_entries (+2) |
| `src/extensions/config_field.rs` | enum_field, enum_field_carries_variants |

## Entry Points

Start here when exploring this area:

- **`run`** (Function) — `src/cli_extension.rs:15`
- **`from_toml_str`** (Function) — `src/extensions/manifest.rs:79`
- **`new`** (Function) — `src/extensions/policy.rs:110`
- **`set_override`** (Function) — `src/extensions/policy.rs:114`
- **`decide`** (Function) — `src/extensions/policy.rs:129`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `run` | Function | `src/cli_extension.rs` | 15 |
| `from_toml_str` | Function | `src/extensions/manifest.rs` | 79 |
| `new` | Function | `src/extensions/policy.rs` | 110 |
| `set_override` | Function | `src/extensions/policy.rs` | 114 |
| `decide` | Function | `src/extensions/policy.rs` | 129 |
| `get` | Function | `src/extensions/registry.rs` | 93 |
| `load_from_store` | Function | `src/extensions/registry.rs` | 192 |
| `load_from_store_with_version` | Function | `src/extensions/registry.rs` | 202 |
| `drafts` | Function | `src/skills/mod.rs` | 223 |
| `parse_skill_extension` | Function | `src/extensions/draft.rs` | 27 |
| `verify_checksum_optional` | Function | `src/extensions/verify.rs` | 74 |
| `discover` | Function | `src/extensions/store.rs` | 43 |
| `upsert` | Function | `src/extensions/registry.rs` | 65 |
| `list` | Function | `src/extensions/registry.rs` | 89 |
| `mark_broken` | Function | `src/extensions/registry.rs` | 124 |
| `active_with_capability` | Function | `src/extensions/registry.rs` | 101 |
| `new` | Function | `src/extensions/audit.rs` | 38 |
| `new` | Function | `src/extensions/audit.rs` | 76 |
| `record` | Function | `src/extensions/audit.rs` | 80 |
| `reset` | Function | `src/extensions/audit.rs` | 107 |

## Execution Flows

| Flow | Type | Steps |
|------|------|-------|
| `Run → DiscoveredExtension` | cross_community | 6 |
| `Run → Sort_in_place` | cross_community | 6 |
| `Run → Get` | cross_community | 5 |
| `Run → InstallState` | intra_community | 5 |
| `Run → Dirs_or_default` | cross_community | 4 |
| `Run → Initialize` | cross_community | 4 |
| `Run_tui → Default` | cross_community | 3 |
| `Run → List` | cross_community | 3 |

## Connected Areas

| Area | Connections |
|------|-------------|
| Config | 5 calls |
| Hooks | 2 calls |
| Code | 2 calls |
| Policy | 2 calls |
| Tui | 1 calls |

## How to Explore

1. `gitnexus_context({name: "run"})` — see callers and callees
2. `gitnexus_query({query: "extensions"})` — find related execution flows
3. Read key files listed above for implementation details
