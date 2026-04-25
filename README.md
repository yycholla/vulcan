# Vulcan

> A Rust AI agent — forged at the forge, tested by fire.

Vulcan is a pure-Rust personal AI agent for the command line. It combines an interactive TUI chat interface with a tool-calling LLM backend, designed for speed, portability, and deep extensibility.

Built on a **hook-driven architecture** — every lifecycle point (prompt assembly, tool dispatch, session boundaries) is an extension surface for audit, safety, skills injection, and custom behavior.

## Features

### 🖥️ Terminal UI
- **Multi-view TUI** — 5 ratatui views (Trading Floor, Single Stack, Split Sessions, Tiled Mesh, Tree of Thought) with smooth keyboard navigation
- **Markdown rendering** — agent responses render inline code, lists, headings, and more
- **Reasoning trace** — toggle visibility of model reasoning/thinking traces inline
- **Prompt mode state machine** — Insert / Command / Ask / Busy modes with per-mode key binding hints
- **Slash commands** — `/help`, `/clear`, `/view`, `/reasoning`, `/search`, `/exit` with fuzzy filter + tab completion + palette navigation
- **Prompt queue** — queue inputs while the agent is busy; drained automatically when the turn completes
- **Live tool activity** — see tool calls start/complete in real-time with ✓/✗ status
- **Live edit diffs** — real file-edit diffs rendered in the UI (not demo data)
- **Live telemetry** — per-session token counts, estimated cost (from provider pricing), tool/error counters, elapsed session time
- **Auto-scroll** — viewport follows the latest content; pauses on manual scroll, re-engages on new submission

### 🤖 Agent Capabilities
- **Interactive chat** — multi-turn conversation with context management and history
- **One-shot mode** — `vulcan prompt "your question"` for scripting and pipelines
- **Session persistence** — SQLite-backed storage with FTS5 full-text search across all sessions
- **Session resume** — resume the last session or a specific session by ID
- **Cross-session search** — `vulcan search "query"` for BM25-ranked full-text search across saved conversations
- **Session lineage** — parent-session tracking for branching conversation trees

### 🛠️ Tool System
Seven built-in tools with schema validation and required-field checking:

| Tool | Description |
|------|-------------|
| `read_file` | Read files with optional offset/limit |
| `write_file` | Write/create files (captures diff) |
| `edit_file` | Find-and-replace edits (captures diff) |
| `search_files` | Ripgrep-style regex search |
| `bash` | One-shot shell command execution (PTY-backed) |
| `pty_create` / `pty_write` / `pty_read` / `pty_resize` / `pty_close` / `pty_list` | Persistent interactive PTY sessions |
| `web_search` | DuckDuckGo web search |
| `web_fetch` | URL content fetch (markdown extraction) |

### 🔌 Hook System (Pi-style Extension Surface)
Seven wire-in points — every hook has priority ordering, timeout isolation, and error containment:

| Event | Purpose |
|-------|---------|
| `BeforePrompt` | Inject transient messages (skills, system prompts) |
| `BeforeToolCall` | Block or modify tool arguments (safety gate) |
| `AfterToolCall` | Inspect or replace tool results |
| `BeforeAgentEnd` | Force the agent loop to continue |
| `session_start` | Lifecycle — session opened |
| `session_end` | Lifecycle — session closed |

**Built-in hooks:**
- **SafetyHook** — blocks dangerous shell commands (rm -rf /, dd, mkfs, chmod 777, fork bombs, force pushes, curl|bash) with interactive approval (allow once / remember & allow / deny) via inline action pills
- **AuditHook** — ring-buffered tool-call audit log, surfaced in the TUI tool-log pane
- **SkillsHook** — injects available skills into the prompt at startup

### ⏸️ AgentPause Mechanism
Generic mid-loop user-interruption system. When a hook needs user input (safety approval, tool confirmation, skill-save prompt), it emits an `AgentPause` with inline action pills. The TUI renders an overlay, dispatches keystrokes back via oneshot channels, and the agent resumes — all without blocking other futures.

### 🔗 LLM Provider
- **OpenAI-compatible** — works with OpenRouter, Anthropic, OpenAI, Ollama, DeepSeek, any OpenAI-compatible endpoint
- **Streaming** — SSE-based streaming with text, reasoning, and tool call events
- **Retry logic** — exponential backoff with jitter (1s, 2s, 4s, 8s, 16s) for 429/5xx/network errors
- **Structured error taxonomy** — `Auth`, `RateLimited`, `ModelNotFound`, `BadRequest`, `ServerError`, `Network` — each with actionable user-facing messages
- **Reasoning passthrough** — supports DeepSeek `reasoning_content` with dual-field serialization (`reasoning_content` + `reasoning`) for OpenRouter compatibility
- **Provider model catalog** — auto-fetches model metadata at startup, validates model exists, fuzzy-suggests alternatives, auto-populates `context_length` and pricing

### ⚙️ Configuration
TOML config at `~/.vulcan/config.toml` (or `./config.toml`):

| Setting | Default | Description |
|---------|---------|-------------|
| `provider.type` | `openai-compat` | Provider type |
| `provider.base_url` | `https://openrouter.ai/api/v1` | API base URL |
| `provider.model` | `deepseek/deepseek-v4-flash` | Model identifier |
| `provider.max_context` | `128000` | Max context tokens |
| `provider.max_retries` | `4` | Transient error retries |
| `provider.catalog_cache_ttl_hours` | `24` | Model catalog cache lifetime |
| `provider.disable_catalog` | `false` | Skip catalog fetch at startup |
| `provider.debug` | `"off"` | Debug logging level (`off`, `tool-fallback`, `wire`) |
| `tools.yolo_mode` | `false` | Skip safety confirmations |
| `compaction.enabled` | `true` | Auto-compress context at threshold |
| `compaction.trigger_ratio` | `0.85` | Compaction trigger ratio |
| `compaction.reserved_tokens` | `50000` | Reserved tokens for response |

API key: set `VULCAN_API_KEY` env var or add `provider.api_key` to config.

### 📦 Skill System
Markdown-based skill registry with YAML frontmatter, loaded as prompt injections via the SkillsHook. Skills live in `~/.vulcan/skills/` and auto-create when the agent detects repeated tool patterns.

## Quick Start

```bash
# Build
cargo build --release

# Run TUI (default)
vulcan

# One-shot prompt
vulcan prompt "What is the capital of France?"

# Resume last session
vulcan --continue

# Resume specific session
vulcan session <session-id>

# Search past sessions
vulcan search "some query"
```

The TUI uses ratatui with crossterm — it works on any terminal that supports an alternate screen.

### TUI Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `Enter` | Send prompt / run command |
| `Esc` | Cancel / deny pause |
| `Ctrl+1..5` | Switch views (1=Single Stack, 5=Trading Floor) |
| `Ctrl+T` | Focus tools view |
| `Ctrl+K` | Focus sessions view |
| `Ctrl+Backspace` | Drop last queued prompt |
| `Ctrl+Shift+Backspace` | Clear entire prompt queue |
| `Ctrl+C` | Cancel in-flight agent turn |
| `Tab` | Complete slash command |
| `↑↓` or `Ctrl+J/K` | Navigate slash command palette |
| `y` / `n` / `r` | Allow / deny / remember (pause prompts) |

## Architecture

```
main.rs ──► Cli ──► Chat (TUI) ──► Agent ──► Provider ──► LLM API
                    │                 │
                    │            HookRegistry
                    │              ├─ safety (blocks dangerous commands)
                    │              ├─ audit (ring-buffered tool log)
                    │              ├─ skills (prompt injections)
                    │              └─ (user-defined)
                    │
                    │           AgentPause channel
                    │              └─ SafetyApproval / ToolArgConfirm / SkillSave
                    │
                 ToolSet
                  ├─ file (read, write, search, edit)
                  ├─ shell/pty (bash, pty_create/write/read/resize/close/list)
                  └─ web (search, fetch)
```

## Project Structure

```
src/
├── main.rs             Entry point (CLI dispatch, logging init)
├── lib.rs              Module tree
├── cli.rs              CLI argument parsing (clap) — chat, prompt, session, search
├── config.rs           TOML config loader, vulcan_home(), API key resolution
├── agent.rs            Core agent loop (tool dispatch, hook wiring, streaming)
├── context.rs          Context window management (token tracking, compaction)
├── prompt_builder.rs   System prompt construction
├── memory.rs           SQLite session store, FTS5 search, lineage tracking
├── pause.rs            AgentPause mechanism (generic mid-loop user interruption)
├── hooks/
│   ├── mod.rs          Hook trait, outcomes, registry (priority, timeout)
│   ├── audit.rs        Built-in audit hook (ring-buffered tool call log)
│   ├── safety.rs       Built-in safety hook (dangerous command blocking)
│   └── skills.rs       Skills-as-hooks injection
├── platform/
│   └── mod.rs          Platform abstraction
├── provider/
│   ├── mod.rs          Provider trait, error taxonomy, message types, streaming events
│   ├── openai.rs       OpenAI-compatible streaming implementation (SSE, retry, reasoning)
│   ├── catalog.rs      Model catalog fetcher (OpenRouter-rich / OpenAI-sparse, caching, fuzzy suggest)
│   └── mock.rs         Test mock provider
├── skills/
│   └── mod.rs          Skill registry (markdown + YAML frontmatter)
├── tools/
│   ├── mod.rs          Tool trait, registry, schema validation, EditDiff
│   ├── file.rs         ReadFile, WriteFile, SearchFiles, PatchFile (with diff capture)
│   ├── shell.rs        Bash + PTY session management (create/write/read/resize/close/list)
│   └── web.rs          Web search (DuckDuckGo) and fetch tools
└── tui/
    ├── mod.rs          TUI loop, key dispatch, pause handling, slash commands
    ├── state.rs        App state, ChatMessage, PromptMode, orchestration state
    ├── markdown.rs     Markdown-to-ratatui parser
    ├── theme.rs        Color palette and styles
    ├── views.rs        Five view renderers (TradingFloor, SingleStack, SplitSessions, etc.)
    └── widgets.rs      Custom widgets (frame, section_header, sparkline, ticker, etc.)
```

## Building

```bash
cargo build             # Debug build
cargo build --release   # Optimized release (size-optimized: LTO, strip)
cargo test              # Run all tests
cargo test <name>       # Single test
```

Set `VULCAN_LOG=debug` for verbose logging; `VULCAN_LOG=trace` for wire-level provider debugging.

## Roadmap

**Current** — core agent, multi-view TUI, SQLite persistence with FTS5, tool system (file/shell/pty/web), hook system (safety/audit/skills), provider model catalog, streaming with reasoning passthrough, inline segment timeline, prompt queue, live cost/telemetry, edit diffs, session lineage.

**Planned** — context compaction with LLM summarization, external hook handlers (Python/JS), platform connectors (Discord, Telegram), gateway daemon, cron scheduling, sub-agent orchestration.

Tracked in Linear: [Vulcan — Rust AI Agent](https://linear.app/yycholla/project/vulcan-rust-ai-agent-37bc34d04e48)

## License

MIT
