---
name: routes
description: "Skill for the Routes area of vulcan. 41 symbols across 10 files."
---

# Routes

41 symbols | 10 files | Cohesion: 69%

## When to Use

- Working with code in `src/`
- Understanding how empty, build_router, with_webhook_secret work
- Modifying routes-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/gateway/routes/inbound.rs` | fresh_db, no_daemon_router, app_state_with, registry_with_loopback, auth_request (+4) |
| `src/gateway/routes/scheduler.rs` | fresh_db, job, build_app_state, get_scheduler_returns_empty_when_no_jobs, get_scheduler_lists_configured_jobs_without_runs (+3) |
| `src/gateway/routes/webhook.rs` | fresh_db, sign_loopback, no_daemon_router, app_state_with, webhook_loopback_accepts_signed_request (+3) |
| `src/gateway/server.rs` | build_router, test_app_state, health_endpoint_no_auth, bearer_required_returns_401_when_missing, bearer_wrong_token_returns_401 (+1) |
| `src/gateway/routes/lanes.rs` | fresh_db, no_daemon_router, build_app_state, get_lanes_empty_cache_returns_empty_array |
| `src/skills/mod.rs` | empty, empty_registry_has_no_skills |
| `tests/contracts.rs` | empty_skills |
| `tests/agent_loop.rs` | empty_skills |
| `src/code/embed.rs` | yyc216_empty_excluder_matches_nothing |
| `src/gateway/loopback.rs` | with_webhook_secret |

## Entry Points

Start here when exploring this area:

- **`empty`** (Function) — `src/skills/mod.rs:115`
- **`build_router`** (Function) — `src/gateway/server.rs:44`
- **`with_webhook_secret`** (Function) — `src/gateway/loopback.rs:43`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `empty` | Function | `src/skills/mod.rs` | 115 |
| `build_router` | Function | `src/gateway/server.rs` | 44 |
| `with_webhook_secret` | Function | `src/gateway/loopback.rs` | 43 |
| `empty_skills` | Function | `tests/contracts.rs` | 52 |
| `empty_skills` | Function | `tests/agent_loop.rs` | 44 |
| `empty_registry_has_no_skills` | Function | `src/skills/mod.rs` | 498 |
| `test_app_state` | Function | `src/gateway/server.rs` | 134 |
| `health_endpoint_no_auth` | Function | `src/gateway/server.rs` | 148 |
| `bearer_required_returns_401_when_missing` | Function | `src/gateway/server.rs` | 163 |
| `bearer_wrong_token_returns_401` | Function | `src/gateway/server.rs` | 178 |
| `bearer_correct_token_passes` | Function | `src/gateway/server.rs` | 194 |
| `yyc216_empty_excluder_matches_nothing` | Function | `src/code/embed.rs` | 464 |
| `fresh_db` | Function | `src/gateway/routes/scheduler.rs` | 98 |
| `job` | Function | `src/gateway/routes/scheduler.rs` | 102 |
| `build_app_state` | Function | `src/gateway/routes/scheduler.rs` | 117 |
| `get_scheduler_returns_empty_when_no_jobs` | Function | `src/gateway/routes/scheduler.rs` | 142 |
| `get_scheduler_lists_configured_jobs_without_runs` | Function | `src/gateway/routes/scheduler.rs` | 165 |
| `get_scheduler_merges_run_history` | Function | `src/gateway/routes/scheduler.rs` | 195 |
| `get_scheduler_disabled_jobs_have_no_next_fire` | Function | `src/gateway/routes/scheduler.rs` | 225 |
| `get_scheduler_requires_bearer` | Function | `src/gateway/routes/scheduler.rs` | 249 |

## Execution Flows

| Flow | Type | Steps |
|------|------|-------|
| `Webhook_loopback_accepts_signed_request → Connect` | cross_community | 9 |
| `Post_inbound_enqueues_and_returns_id → Connect` | cross_community | 9 |
| `Webhook_loopback_rejects_invalid_signature → Connect` | cross_community | 9 |
| `Webhook_rejects_oversized_body_before_verification → Connect` | cross_community | 9 |
| `Post_inbound_unknown_platform_returns_400 → Connect` | cross_community | 9 |
| `Post_inbound_missing_bearer_returns_401 → Connect` | cross_community | 9 |
| `Webhook_loopback_accepts_signed_request → Dirs_or_default` | cross_community | 8 |
| `Post_inbound_db_error_returns_503 → Apply_connection_pragmas` | cross_community | 8 |
| `Get_scheduler_merges_run_history → Connect` | cross_community | 8 |
| `Get_lanes_empty_cache_returns_empty_array → Connect` | cross_community | 8 |

## Connected Areas

| Area | Connections |
|------|-------------|
| Gateway | 20 calls |
| Daemon | 4 calls |
| Tui | 1 calls |
| Agent | 1 calls |

## How to Explore

1. `gitnexus_context({name: "empty"})` — see callers and callees
2. `gitnexus_query({query: "routes"})` — find related execution flows
3. Read key files listed above for implementation details
