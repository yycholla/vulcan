# Vulcan

> A Rust AI agent — forged at the forge, tested by fire.

Vulcan (formerly Ferris) is a pure-Rust personal AI agent for the command line. It combines an interactive TUI chat interface with a tool-calling LLM backend, designed for speed, portability, and deep extensibility.

## Features

- **Interactive TUI** — ratatui-based terminal UI with chat message panel, input bar, markdown rendering, and thinking indicator
- **One-shot mode** — `vulcan prompt "your question"` for scripting and pipelines
- **Session persistence** — conversations saved to JSONL with resume support (`vulcan session <id>`)
- **Tool calling** — file system operations, shell commands, and web search/fetch
- **Hook system** — Pi-style 5-event extension surface (`BeforePrompt`, `BeforeToolCall`, `AfterToolCall`, `BeforeAgentEnd`, `session_start/session_end`) for audit, skill injection, and safety gating
- **Skill system** — markdown-based skill registry with YAML frontmatter, loaded as prompt injections
- **LLM provider** — OpenAI-compatible streaming provider supporting OpenRouter, Anthropic, Ollama, and any OpenAI-compatible endpoint
- **Configurable** — TOML config (`~/.vulcan/config.toml`) with env-var API key support
- **Logging** — tracing-based, writes to file in TUI mode / stderr in CLI mode

## Quick Start

```bash
# Build
cargo build --release

# Run TUI (default)
vulcan

# One-shot prompt
vulcan prompt "What is the capital of France?"

# Resume a session
vulcan session <session-id>
```

The TUI uses ratatui with crossterm — it works on any terminal that supports an alternate screen.

## Configuration

Config lives at `~/.vulcan/config.toml`. See `config.example.toml` for all options.

| Setting | Default | Description |
|---------|---------|-------------|
| `provider.type` | `openai-compat` | Provider type |
| `provider.base_url` | `https://openrouter.ai/api/v1` | API base URL |
| `provider.model` | `deepseek/deepseek-v4-flash` | Model identifier |
| `provider.max_context` | `128000` | Max context tokens |
| `tools.yolo_mode` | `false` | Skip safety confirmations |
| `compaction.enabled` | `true` | Auto-compress context at threshold |

API key: set `VULCAN_API_KEY` env var or add `provider.api_key` to config.

## Architecture

```
main.rs ──► Cli ──► Chat (TUI) ──► Agent ──► Provider ──► LLM API
                    │                 │
                    │            HookRegistry
                    │              ├─ audit
                    │              ├─ skills
                    │              └─ (user-defined)
                    │
                 ToolSet
                  ├─ file (read, write, search, edit)
                  ├─ shell (bash execution)
                  └─ web (search, fetch)
```

The **hook system** is the foundational extension surface:

- `BeforePrompt` — inject messages (skills, system prompts)
- `BeforeToolCall` — block or modify tool arguments (safety gate)
- `AfterToolCall` — inspect or replace tool results
- `BeforeAgentEnd` — final processing before returning to user
- `session_start` / `session_end` — lifecycle hooks

All hooks support both streaming and buffered LLM paths.

## Project Structure

```
src/
├── main.rs            Entry point
├── lib.rs             Module tree
├── cli.rs             CLI argument parsing (clap)
├── config.rs          TOML config loader
├── agent.rs           Core agent loop (tool dispatch, hook wiring)
├── context.rs         Context window management
├── prompt_builder.rs  System prompt construction
├── memory.rs          Cross-session memory
├── hooks/
│   ├── mod.rs         Hook trait, outcomes, registry
│   ├── audit.rs       Built-in audit hook (tool call logging)
│   └── skills.rs      Skills-as-hooks injection
├── platform/
│   └── mod.rs         Platform abstraction
├── provider/
│   ├── mod.rs         Provider trait
│   └── openai.rs      OpenAI-compatible streaming implementation
├── tools/
│   ├── mod.rs         Tool trait
│   ├── file.rs        File system tools
│   ├── shell.rs       Bash/PTY execution
│   └── web.rs         Web search and fetch
├── skills/
│   └── mod.rs         Skill registry (markdown + YAML frontmatter)
└── tui/
    ├── mod.rs         TUI loop and rendering
    ├── markdown.rs    Markdown-to-ratatui parser
    ├── state.rs       TUI state management
    ├── theme.rs       Visual theme
    ├── views.rs       Layout components
    └── widgets.rs     Custom widgets
```

## Building

```bash
cargo build             # Debug build
cargo build --release   # Optimized release (size-optimized: LTO, strip)
cargo test              # Run tests
cargo test <name>       # Single test
```

Set `VULCAN_LOG=debug` for verbose logging.

## Roadmap

Phase 1 (current) — core agent, tools, TUI, hooks, skills, config, JSONL persistence
Phase 2 — SQLite session store, FTS5 search, context compaction, cron scheduling
Phase 3 — external hook handlers, platform connectors (Discord, Telegram), gateway daemon

Tracked in Linear: [Vulcan — Rust AI Agent](https://linear.app/yycholla/project/vulcan-rust-ai-agent-37bc34d04e48)

## License

MIT
