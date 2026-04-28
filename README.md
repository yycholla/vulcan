# Vulcan

> Vulcan's hammer, Rust's steel.
> 
> *Command AI like a tool, not a conversation.*

**Vulcan** is a pure-Rust AI agent that lives in your terminal. It gives you a powerful, interactive LLM-powered assistant with file editing, shell access, web search, persistent sessions, and a beautiful TUI — all in a single binary.

```bash
# Start the TUI
vulcan

# One-shot prompt
vulcan prompt "Find all TODO comments in this project"

# Resume your last session
vulcan --continue

# Full-text search across all past sessions
vulcan search "some query"

# Run the gateway daemon (requires the gateway feature)
cargo run --features gateway -- gateway
```

---

## Features

### 🖥️ Terminal UI (TUI)

Vulcan's ratatui-powered terminal interface gives you five views to work with:

| View | What it shows |
|------|---------------|
| **Single Stack** | A focused chat view — you and the agent, one conversation at a time |
| **Split Sessions** | Chat on the left, session list/side panel on the right |
| **Tiled Mesh** | Grid layout showing chat, tool activity, sessions, and telemetry |
| **Trading Floor** | All panels at once — see everything happening in real-time |
| **Tree of Thought** | Branching conversation trees for exploring multiple lines of reasoning |

Switch between views instantly with `Ctrl+1` through `Ctrl+5`.

**UI highlights:**
- **Markdown rendering** — agent responses render inline code, lists, headings, and more
- **Reasoning trace** — toggle visibility of model reasoning/thinking traces (`Ctrl+R`)
- **Slash commands** — `/help`, `/clear`, `/view`, `/reasoning`, `/model`, `/search`, `/exit` with fuzzy filtering, tab completion, and a navigable palette
- **Prompt queue** — keep typing while the agent is busy; prompts drain automatically when the turn completes
- **Live tool activity** — see tool calls start and complete in real-time with ✓/✗ status
- **Live edit diffs** — real file-edit diffs rendered in the UI as the agent works
- **Live telemetry** — per-session token counts, estimated cost, tool/error counters, elapsed time
- **Auto-scroll** — viewport follows content; pauses on manual scroll, resumes on new input

### 🤖 Agent Capabilities

- **Interactive chat** — multi-turn conversations with full context management
- **One-shot mode** — `vulcan prompt "your question"` for scripting and pipelines (streams tokens to stdout)
- **Session persistence** — all conversations saved to SQLite with full-text search (FTS5)
- **Session resume** — pick up where you left off with `vulcan --continue` or `vulcan session <id>`
- **Cross-session search** — `vulcan search "query"` finds relevant messages across your entire history
- **Session lineage** — parent-session tracking for branching conversation trees

### 🛠️ Tool System

The agent can use these tools to help you:

| Tool | What it does |
|------|-------------|
| `read_file` | Read files with optional offset/limit |
| `write_file` | Write or create files (captures a diff) |
| `edit_file` | Find-and-replace edits with fuzzy matching (captures a diff) |
| `search_files` | Ripgrep-style regex search across your codebase |
| `bash` | Execute shell commands (PTY-backed) |
| `pty_*` | Full interactive PTY session management (create, write, read, resize, close, list) |
| `web_search` | DuckDuckGo web search |
| `web_fetch` | Fetch a URL and extract its content as markdown |

### 🔒 Safety & Audit

- **Safety system** — dangerous shell commands (`rm -rf /`, `dd`, `mkfs`, fork bombs, force pushes, `curl|bash`) are blocked with interactive approval prompts
- **Audit log** — ring-buffered tool-call log viewable in the TUI
- **Action pills** — approve/deny/remember inline responses for safety prompts

### 🧠 Skill System

Vulcan learns. Skills are markdown files with YAML frontmatter that get injected into the system prompt. They live in `~/.vulcan/skills/` and can auto-create when the agent detects repeated tool patterns.

### 🔗 LLM Provider Support

- **OpenAI-compatible** — works with OpenRouter, Anthropic, OpenAI, Ollama, DeepSeek, or any OpenAI-compatible API endpoint
- **Streaming** — SSE-based streaming with text, reasoning, and tool calls
- **Retry logic** — exponential backoff with jitter (1s, 2s, 4s, 8s, 16s) for 429/5xx/network errors
- **Model catalog** — auto-fetches model metadata at startup, validates the model exists, fuzzy-suggests alternatives, auto-populates context length and pricing
- **Named providers** — configure multiple OpenAI-compatible endpoints and switch to catalog-listed models from the TUI
- **Reasoning passthrough** — supports models like DeepSeek that emit `reasoning_content` alongside responses

---

## Installation

### From Source

```bash
# Clone the repo
git clone https://github.com/yycholla/vulcan.git
cd vulcan

# Build release binary (size-optimized: LTO, strip)
cargo build --release

# The binary is at ./target/release/vulcan — copy it somewhere on your PATH
cp ./target/release/vulcan ~/.local/bin/
```

### Prerequisites

- **Rust toolchain** — install via [rustup](https://rustup.rs/)
- An API key from one of the supported LLM providers (see [Configuration](#configuration))

---

## Configuration

Vulcan looks for config at `~/.vulcan/config.toml` (or `./config.toml` in the current directory).

### 1. Create the config

```bash
mkdir -p ~/.vulcan
cp config.example.toml ~/.vulcan/config.toml
```

### 2. Set your API key

Either set the environment variable:

```bash
export VULCAN_API_KEY="sk-..."
```

Or add it to your config file:

```toml
[provider]
api_key = "sk-..."
```

### 3. Choose a model

The default works with OpenRouter (uses `deepseek/deepseek-v4-flash`). Change the `model` and `base_url` to use a different provider:

```toml
[provider]
type = "openai-compat"
base_url = "https://openrouter.ai/api/v1"
model = "deepseek/deepseek-v4-flash"
```

### Full Configuration Reference

| Setting | Default | Description |
|---------|---------|-------------|
| `provider.type` | `openai-compat` | Provider type |
| `provider.base_url` | `https://openrouter.ai/api/v1` | API base URL |
| `provider.model` | `deepseek/deepseek-v4-flash` | Model identifier |
| `provider.max_context` | `128000` | Max context tokens |
| `provider.max_retries` | `4` | Transient error retries |
| `provider.catalog_cache_ttl_hours` | `24` | Model catalog cache lifetime |
| `provider.disable_catalog` | `false` | Skip catalog fetch at startup |
| `provider.debug` | `"off"` | Debug logging: `off`, `tool-fallback`, or `wire` |
| `providers.<name>.*` | unset | Optional named provider profiles usable by `/model <name>/<model>` |
| `gateway.bind` | `127.0.0.1:7373` | Gateway HTTP bind address |
| `gateway.api_token` | unset | Bearer token required for `/v1/*` gateway routes |
| `gateway.idle_ttl_secs` | `1800` | Per-lane agent idle eviction timeout |
| `gateway.outbound_max_attempts` | `5` | Outbound delivery retries before failure |
| `gateway.discord.enabled` | `false` | Enable Discord gateway connector |
| `gateway.discord.bot_token` | unset | Discord bot token for Serenity |
| `gateway.discord.allow_bots` | `false` | Allow bot-authored Discord messages into the agent queue |
| `tools.yolo_mode` | `false` | Skip safety confirmations |
| `compaction.enabled` | `true` | Auto-compress context at threshold |
| `compaction.trigger_ratio` | `0.85` | Compaction trigger ratio |
| `compaction.reserved_tokens` | `50000` | Reserved tokens for response |

---

## Usage

### Commands

```bash
# Start interactive TUI (default)
vulcan

# One-shot mode — ask a question, get an answer
vulcan prompt "What is the capital of France?"

# Resume the most recent session
vulcan --continue

# Resume a specific session by ID
vulcan session <session-id>

# Full-text search across all saved sessions
vulcan search "some query"
# Optionally limit results: vulcan search "query" --limit 20

# Gateway daemon mode
cargo run --features gateway -- gateway
```

### TUI Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `Enter` | Send prompt / run command |
| `Esc` | Cancel / deny pause prompt |
| `Ctrl+1` to `Ctrl+5` | Switch views (1=Single Stack, 5=Trading Floor) |
| `Ctrl+T` | Focus tools/log view |
| `Ctrl+K` | Focus sessions view |
| `Ctrl+R` | Toggle reasoning trace visibility |
| `Ctrl+Backspace` | Drop the last queued prompt |
| `Ctrl+Shift+Backspace` | Clear the entire prompt queue |
| `Ctrl+C` | Cancel an in-flight agent turn |
| `Tab` | Complete a slash command |
| `↑` / `↓` or `Ctrl+J` / `Ctrl+K` | Navigate the slash command palette |
| `y` / `n` / `r` | Allow / deny / remember (safety pause prompts) |

### Slash Commands

Type `/` in the TUI to access slash commands:

```
/help       Show help information
/clear      Clear the conversation
/view       Switch views (equivalent to Ctrl+1..5)
/reasoning  Toggle reasoning trace
/model      List available models or switch with /model <id>
/search     Search past sessions
/exit       Quit Vulcan
```

Use `Tab` to autocomplete, `↑`/`↓` to navigate suggestions.

### Logging

| Env Var | Effect |
|---------|--------|
| `VULCAN_LOG=info` | Default logging |
| `VULCAN_LOG=debug` | Verbose debugging |
| `VULCAN_LOG=trace` | Wire-level provider debugging |

In TUI mode, logs go to `~/.vulcan/vulcan.log`. In one-shot/CLI mode, logs go to stderr.

---

## Architecture (for the curious)

```
main.rs ──► Cli ──► Chat (TUI) ──► Agent ──► Provider ──► LLM API
                    │                 │
                    │            HookRegistry
                    │              ├─ safety (blocks dangerous commands)
                    │              ├─ audit (ring-buffered tool log)
                    │              ├─ skills (prompt injections)
                    │              └─ (user-extensible)
                    │
                    │           AgentPause channel
                    │              └─ SafetyApproval / ToolArgConfirm / SkillSave
                    │
                 ToolSet
                  ├─ file (read, write, search, edit)
                  ├─ shell/pty (bash, pty sessions)
                  └─ web (search, fetch)
```

Vulcan is built on a **hook-driven architecture** — every lifecycle point (prompt assembly, tool dispatch, session boundaries) is an extension surface for audit, safety, skills injection, and custom behavior.

---

## Development

```bash
cargo build               # Debug build
cargo build --release     # Optimized release (size-optimized)
cargo test                # Run all tests
cargo test <name>         # Run a single test by name
```

---

## Roadmap

**Current** — core agent, multi-view TUI, SQLite persistence with FTS5, tool system (file/shell/pty/web), hook system (safety/audit/skills), provider model catalog, streaming with reasoning passthrough, inline segment timeline, prompt queue, live cost/telemetry, edit diffs, session lineage, gateway daemon, Discord connector scaffold.

**Planned** — context compaction with LLM summarization, external hook handlers (Python/JS), Telegram connector, richer Discord controls, cron scheduling, sub-agent orchestration.

Tracked in [Linear — Vulcan: Rust AI Agent](https://linear.app/yycholla/project/vulcan-rust-ai-agent-37bc34d04e48).

---

## License

MIT
