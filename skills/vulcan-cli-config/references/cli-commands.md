# Vulcan CLI Command Reference

## Top-Level

| Command | Description |
|---|---|
| `vulcan` (no args) | Launch TUI chat |
| `vulcan <text>` | One-shot prompt, stream response to stdout |
| `vulcan --continue` | Resume last session in TUI |
| `vulcan --resume` | Session picker in TUI |
| `vulcan --profile <name>` | Override tool capability profile for this run |

## Subcommands

### `auth [preset]`
Interactive guided setup. Fuzzy-selects a provider preset, prompts for name/key/model, delegates to `provider add`.

### `completions <shell>`
Generate shell completions (bash, zsh, fish, etc.) via `clap_complete`.

### `config`
| Subcommand | Flags | Description |
|---|---|---|
| `list` | — | All fields with types and defaults |
| `get <key>` | `--reveal` | Read one value; redacts secrets unless `--reveal` |
| `set <key> <value>` | — | Write one value (preserves TOML comments) |
| `unset <key>` | — | Remove override, revert to default |
| `path` | — | Print config directory path |
| `show` | `--reveal` | Dump full resolved config |

### `doctor`
Runs config/storage/workspace/tool-registry diagnostics. Runs *before* `Config::load()` so it can diagnose broken configs.

### `extension`
| Subcommand | Flags | Description |
|---|---|---|
| `list` | — | List installed extensions |
| `show <id>` | — | Show extension details |
| `enable <id>` | — | Enable an extension |
| `disable <id>` | — | Disable an extension |
| `uninstall <id>` | `--yes` | Remove an extension |
| `new <name>` | `--kind <prompt\|rust>` | Scaffold a new extension |
| `validate <path>` | — | Validate an extension manifest |
| `install <path>` | — | Install extension from path |

### `gateway` *(feature-gated)*
| Subcommand | Flags | Description |
|---|---|---|
| `init` | `--force` | Bootstrap `[gateway]` in config.toml + generate API token |
| `run` | `--bind <addr>` | Start the axum gateway daemon |

### `impact <target>`
Change-impact report for a file (walks code refs + tests + docs).

| Flag | Description |
|---|---|
| `--save` | Persist report as artifact |

### `knowledge`
| Subcommand | Flags | Description |
|---|---|---|
| `list` | — | List local knowledge indexes |
| `purge` | `--kind`, `--workspace`, `--all`, `--yes` | Purge knowledge indexes |

### `migrate-config [--force]`
Split monolithic config.toml into config + keybinds + providers files. Idempotent; creates `.bak` snapshots for rollback.

### `model`
| Subcommand | Flags | Description |
|---|---|---|
| `list` | — | Query provider `/models` catalog |
| `show` | — | Show active model |
| `use <id>` | `--force` | Persist model selection on active provider profile |

### `playbook`
| Subcommand | Description |
|---|---|
| `list [--status]` | List project playbooks |
| `show <id>` | Show playbook entry |
| `accept <id>` | Accept a proposed playbook |
| `remove <id>` | Remove a playbook |
| `import [--path]` | Import `AGENTS.md`/`CLAUDE.md`/`README.md` as proposed entries |

### `policy simulate [path] [--profile <name>]`
Dry-run effective tool policy for a workspace + capability profile.

### `prompt <text>`
One-shot streaming response to stdout.

### `provider`
| Subcommand | Flags | Description |
|---|---|---|
| `list` | — | List named provider profiles |
| `presets` | — | Show available provider presets |
| `add <name>` | `--preset <name>` | Add a named provider profile |
| `remove <name>` | — | Remove a named provider profile |
| `use <name>` | `--clear` | Activate a profile (`--clear` reverts to legacy) |

Available presets: OpenRouter, OpenAI, Anthropic, DeepSeek, Groq, Together, Fireworks, Ollama.

### `replay inspect <id>`
Read-only render of a saved run's timeline.

### `review`
| Subcommand | Description |
|---|---|
| `plan <target>` | Generate a review plan (accepts file path or `-` for stdin) |
| `diff <target>` | Generate a review diff |
| `run <id>` | Execute a bounded critic pass under `reviewer` profile |

### `run`
| Subcommand | Flags | Description |
|---|---|---|
| `list` | `--limit` | List durable run records |
| `show <id>` | — | Show run details (accepts UUID or 8-char prefix) |

### `artifact`
| Subcommand | Flags | Description |
|---|---|---|
| `list` | `--limit`, `--run`, `--session` | List typed artifacts |
| `show <id>` | — | Show artifact (accepts UUID or 8-char prefix) |

### `search <query>`
Full-text search across all saved sessions via FTS5.

### `session <id>`
Resume a specific session by ID in TUI.

### `trust why [path]`
Explains workspace trust resolution: level, capability_profile, allow_indexing, allow_persistence, reason.
