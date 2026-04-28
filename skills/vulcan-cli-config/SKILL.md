---
name: vulcan-cli-config
description: Navigate and modify the Vulcan CLI commands and TOML configuration. Use when the user asks about CLI subcommands, config keys, provider profiles, model selection, gateway setup, extensions, or any `vulcan` invocation. Don't use for agent-loop internals, TUI rendering, or provider protocol details.
---

# Vulcan CLI & Config

## Quick Start

1. Run `vulcan` with no arguments to launch the TUI chat.
2. Run `vulcan config list` to see every configurable field with defaults.
3. Run `vulcan auth` for guided first-time provider setup.

## Core Workflows

### Configure a Provider

1. Run `vulcan provider add <name> --preset <preset>` to create a named profile.
2. Run `vulcan provider use <name>` to activate it (writes `active_profile` to config.toml).
3. Run `vulcan model use <model-id>` to switch the model on the active profile.
4. For guided interactive setup, run `vulcan auth` — it fuzzy-selects a preset and prompts for credentials.

### Inspect or Change Config

1. `vulcan config list` — all fields, types, defaults.
2. `vulcan config get <dotted.key>` — read one value (`--reveal` for secrets).
3. `vulcan config set <dotted.key> <value>` — write one value (preserves comments).
4. `vulcan config unset <dotted.key>` — remove an override, reverting to default.
5. `vulcan config path` — print the config directory (`~/.vulcan/`).
6. `vulcan config show --reveal` — dump full resolved config.

### Diagnose Problems

1. If the user sees a config-parse or startup error → run `vulcan doctor` (runs before config load, can diagnose broken configs).
2. If the user sees a permission or trust warning → run `vulcan trust why [path]` to explain the resolution.
3. If the user sees a tool-policy denial → run `vulcan policy simulate [--profile <name>]` to dry-run the effective policy.

### Gateway (feature-gated)

1. `vulcan gateway init [--force]` — bootstraps `[gateway]` in config.toml + generates API token.
2. `vulcan gateway run [--bind <addr>]` — starts the axum daemon.

## Config File Layout

Three TOML files under `~/.vulcan/`:

| File | Purpose |
|---|---|
| `config.toml` | Core settings: tools, compaction, recall, embeddings, tui, gateway, scheduler, extensions, workspace_trust, keybinds, active_profile |
| `keybinds.toml` | TUI key bindings (split from config.toml by `migrate-config`) |
| `providers.toml` | Named provider profiles (`[<name>]` tables) + legacy `[provider]` block |

Provider resolution: `active_profile` → `[providers.<name>]` → fallback to legacy `[provider]`.

## Key Conventions

- **Dotted paths**: config keys use dot notation (`tools.native_enforcement`, `compaction.trigger_ratio`).
- **Enum values**: always lowercase (`off`, `warn`, `block`; `prompt`, `block`, `allow`).
- **Secret fields**: `provider.api_key` is redacted by default; pass `--reveal` to see.
- **Comment preservation**: `config set`/`unset` use `toml_edit` so user comments survive.
- **UUID prefix resolution**: run/artifact/playbook IDs accept 8-char prefixes.
- **`migrate-config`**: splits monolithic config.toml into 3 files; idempotent with `.bak` snapshots.

## Error Handling

- If `vulcan config set` rejects a value, check the field type in `references/config-fields.md`.
- If `vulcan doctor` reports a broken config, rename `config.toml` → `config.toml.bak` and re-run `vulcan auth`.
- If `vulcan provider use` fails, verify the profile name exists in `providers.toml`.
- If `vulcan auth` or `vulcan model list` fails with a network error, check `provider.base_url` and `provider.api_key`; for local endpoints (Ollama, etc.), ensure the server is running.
- If `vulcan gateway run` fails at startup, verify `[gateway].api_token` is set (run `vulcan gateway init` if missing).
- If `migrate-config` reports conflicts, it creates `.bak` snapshots — restore from backup and re-run.
- On a fresh install with no `providers.toml`, run `vulcan auth` to bootstrap the first provider profile.

## Detailed References

- Read `references/cli-commands.md` for the full CLI command tree with flags and arguments.
- Read `references/config-fields.md` for every declared config field, type, default, and target file.
