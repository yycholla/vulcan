---
name: artifact
description: "Skill for the Artifact area of vulcan. 40 symbols across 4 files."
---

# Artifact

40 symbols | 4 files | Cohesion: 78%

## When to Use

- Working with code in `src/`
- Understanding how run, from_uuid, as_str work
- Modifying artifact-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/artifact/mod.rs` | from_uuid, as_str, row_to_artifact, get, list_for_run (+23) |
| `src/cli_artifact.rs` | run, list, show, resolve_run_id, resolve_artifact_id (+3) |
| `src/review/runner.rs` | persist_report, persist_report_writes_report_artifact_with_review_source, persist_report_is_a_noop_without_store |
| `src/cli_impact.rs` | run |

## Entry Points

Start here when exploring this area:

- **`run`** (Function) — `src/cli_artifact.rs:11`
- **`from_uuid`** (Function) — `src/artifact/mod.rs:49`
- **`as_str`** (Function) — `src/artifact/mod.rs:91`
- **`inline_text`** (Function) — `src/artifact/mod.rs:149`
- **`with_run_id`** (Function) — `src/artifact/mod.rs:165`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `run` | Function | `src/cli_artifact.rs` | 11 |
| `from_uuid` | Function | `src/artifact/mod.rs` | 49 |
| `as_str` | Function | `src/artifact/mod.rs` | 91 |
| `inline_text` | Function | `src/artifact/mod.rs` | 149 |
| `with_run_id` | Function | `src/artifact/mod.rs` | 165 |
| `with_parent` | Function | `src/artifact/mod.rs` | 190 |
| `new` | Function | `src/artifact/mod.rs` | 210 |
| `try_new` | Function | `src/artifact/mod.rs` | 266 |
| `try_open_at` | Function | `src/artifact/mod.rs` | 273 |
| `try_open_in_memory` | Function | `src/artifact/mod.rs` | 283 |
| `run` | Function | `src/cli_impact.rs` | 8 |
| `persist_report` | Function | `src/review/runner.rs` | 57 |
| `with_session_id` | Function | `src/artifact/mod.rs` | 170 |
| `with_source` | Function | `src/artifact/mod.rs` | 175 |
| `with_title` | Function | `src/artifact/mod.rs` | 180 |
| `with_redaction` | Function | `src/artifact/mod.rs` | 185 |
| `list` | Function | `src/cli_artifact.rs` | 23 |
| `show` | Function | `src/cli_artifact.rs` | 56 |
| `resolve_run_id` | Function | `src/cli_artifact.rs` | 114 |
| `row_to_artifact` | Function | `src/artifact/mod.rs` | 319 |

## Execution Flows

| Flow | Type | Steps |
|------|------|-------|
| `Run → Active_provider_config` | cross_community | 6 |
| `Run → New` | cross_community | 5 |
| `Run → Grammar` | cross_community | 5 |
| `Run → From_path` | cross_community | 4 |
| `Run → Outline_query` | cross_community | 4 |
| `Run → Next` | cross_community | 4 |
| `Run → Dirs_or_default` | cross_community | 4 |
| `Run → Initialize` | cross_community | 4 |
| `Run → With_source` | cross_community | 4 |
| `Run → With_title` | cross_community | 4 |

## Connected Areas

| Area | Connections |
|------|-------------|
| Impact | 2 calls |
| Config | 1 calls |

## How to Explore

1. `gitnexus_context({name: "run"})` — see callers and callees
2. `gitnexus_query({query: "artifact"})` — find related execution flows
3. Read key files listed above for implementation details
