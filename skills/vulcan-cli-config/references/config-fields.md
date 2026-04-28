# Vulcan Config Field Reference

All fields declared in `config_registry::BUILTIN_FIELDS`. Each field routes to one of three TOML files under `~/.vulcan/`.

## Config File Routing

| `ConfigFile` variant | Target file |
|---|---|
| `Config` | `config.toml` |
| `Keybinds` | `keybinds.toml` |
| `Providers` | `providers.toml` |

## Field Types

| `FieldKind` | TOML representation | Notes |
|---|---|---|
| `Bool` | `true` / `false` | — |
| `Int { min, max }` | Integer | Bounds are inclusive; `None` = unbounded |
| `Float { min, max }` | Float | Bounds are inclusive; `None` = unbounded |
| `Enum { variants }` | String (one of listed) | Always lowercase |
| `String { secret }` | String | `secret = true` → redacted by default |
| `Path` | String | UTF-8 path; existence not checked |

## Declared Fields

| Dotted Path | Type | Default | File | Help |
|---|---|---|---|---|
| `active_profile` | String | `(unset)` | Config | Persisted active provider profile name |
| `auto_create_skills` | Bool | `false` | Config | After 5+ tool iterations, ask the model to summarize as a draft skill |
| `skills_dir` | Path | `~/.vulcan/skills` | Config | Directory the skills loader walks at session start |
| `tools.yolo_mode` | Bool | `false` | Config | Disable safety + approval prompts |
| `tools.native_enforcement` | Enum: `off`, `warn`, `block` | `block` | Config | How aggressively to redirect bash to native tools |
| `tools.profile` | String | `(unset)` | Config | Default tool capability profile name |
| `tools.dangerous_commands.policy` | Enum: `prompt`, `block`, `allow` | `prompt` | Config | SafetyHook action on dangerous pattern match |
| `tools.dangerous_commands.quota_per_session` | Int [0, 1000] | `5` | Config | Per-session cap on approved dangerous commands; 0 = unlimited |
| `compaction.enabled` | Bool | `true` | Config | Auto-compact context near max window |
| `compaction.trigger_ratio` | Float [0.0, 1.0] | `0.85` | Config | Token ratio at which compaction fires |
| `compaction.reserved_tokens` | Int [0, 2000000] | `50000` | Config | Tokens reserved for next response (capped at max_context/4) |
| `recall.enabled` | Bool | `false` | Config | Auto-recall past-session context on first turn |
| `recall.max_hits` | Int [1, 50] | `5` | Config | Max recalled hits injected into prompt |
| `embeddings.enabled` | Bool | `false` | Config | Register embedding-search tools at session start |
| `embeddings.model` | String | `(unset)` | Config | Embedding model id (e.g. `text-embedding-3-small`) |
| `provider.api_key` | String (secret) | `(unset)` | Providers | API key; also settable via `VULCAN_API_KEY` env |
| `provider.base_url` | String | `(unset)` | Providers | OpenAI-compatible API base URL |
| `provider.model` | String | `(unset)` | Providers | Default model id |
| `provider.max_iterations` | Int [0, 10000] | `0` | Providers | Hard cap on agent loop iterations; 0 = unlimited |
| `tui.show_reasoning` | Bool | `true` | Config | Render reasoning trace in TUI |

## Undeclared Config Sections

These sections exist in `Config` struct but have no registry fields yet (accessed via direct TOML editing):

| Section | Key Fields | Notes |
|---|---|---|
| `[tools.approval]` | `default` + per-tool entries | `ApprovalMode`: `always`, `ask`, `session` |
| `[tools.profiles.<name>]` | `native_enforcement`, `approval`, `dangerous_commands` | User-defined tool profiles |
| `[embeddings]` | `base_url`, `api_key`, `dim` | `dim` default: 1536 |
| `[tui]` | `theme` | Values: `system`, `default-light`, `dracula` |
| `[gateway]` | `bind`, `api_token`, `idle_ttl_secs`, `max_concurrent_lanes`, `outbound_max_attempts` | Feature-gated; `validate()` rejects empty tokens |
| `[gateway.telegram]` | `enabled`, `bot_token`, `webhook_secret`, `allowed_chat_ids`, `poll_interval_secs` | `poll_interval_secs` max 120 |
| `[gateway.discord]` | `enabled`, `bot_token`, `allow_bots` | — |
| `[gateway.commands.<name>]` | `kind = "shell"` { `command`, `timeout_secs` } or `kind = "builtin"` { `name` } | Gateway slash commands |
| `[scheduler]` | `jobs` (array of `SchedulerJobConfig`) | — |
| `[scheduler.jobs]` | `name`, `cron`, `prompt`, `timezone`, `runtime_cap_mins`, `overlap_policy`, `enabled` | `OverlapPolicy`: `Skip`, `Cancel`, `Concurrent` |
| `[workspace_trust]` | Trust level + capability_profile resolution | — |
| `[extensions]` | `disabled` (array), `per_extension.<id>.enabled` | `disabled` wins over per-extension `enabled = true` |
| `[keybinds]` | `toggle_sessions`, `toggle_tools`, `toggle_reasoning`, `cancel`, `queue_drop` | Caret sigils (`⌃K`) or `Ctrl+K` syntax |
| `[providers.<name>]` | Same shape as `[provider]` | Named profiles; selected by `active_profile` |
| `[provider]` | `type`, `debug` (`off`/`tool-fallback`/`wire`), `max_retries`, `catalog_cache_ttl_hours`, `disable_catalog`, `max_output_tokens`, `stream_channel_capacity` | Legacy block; fallback when `active_profile` is unset |

## Provider Resolution Chain

```
active_profile set?
  ├─ Yes → lookup [providers.<active_profile>] in providers.toml
  └─ No  → fall back to [provider] in providers.toml
```

`Config::active_provider_config()` implements this at startup.
