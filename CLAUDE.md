# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Vulcan is a pure-Rust personal AI agent (CLI + TUI). The binary is `vulcan`; chat is the default subcommand.

## Github PR flow

- This project uses Graphite for pr stacks and review. Branches should follow linear best practice for best sync between github state and linear

## Where work is tracked and planned

- **Linear** is the source of truth for tasks. Workspace team **Yycholla**, project **Vulcan — Rust AI Agent** — issues use the `YYC-` prefix. Check existing issues before creating new ones; ask before bulk-creating tickets. Group under an epic issue where possible.
- **`~/wiki/queries/rust-hermes-plan.md`** is the master vision (Phase 1 → 2 → 3, locked Phase 1 scope, tool trait + provider trait shapes). Anything that contradicts the plan is either an open question or a documentation lag — don't silently diverge.
- **`~/wiki/queries/`** also holds design docs (e.g. `hooks-design.md` once written) — prefer adding cross-cutting design docs there, not in the repo.

## Build / run / test

```bash
cargo build --all-targets        # compile everything, including tests
cargo run                         # launch TUI (default subcommand: chat)
cargo run -- prompt "your text"   # one-shot mode (no TUI)
cargo run -- session <id>         # resume a saved session
cargo run --features gateway -- gateway  # run gateway daemon mode
cargo build --release             # fast optimized build for daily iteration
cargo build --profile release-dist # size-optimized binary (opt-level=z, lto, strip)
cargo build --profile selfdev      # fastest optimized build for self-mod loops
cargo test                        # run tests
cargo test --features gateway gateway::  # gateway feature tests
cargo test <name_substring>       # single test by name
```

Logging: TUI mode logs to a file (so `tracing` output doesn't splat the screen); one-shot mode logs to stderr. Set `RUST_LOG=debug` for verbose output.

Config: `~/.vulcan/config.toml` (see `config.example.toml`). API key via `VULCAN_API_KEY` env var or the config file.

## Architecture worth reading multiple files to understand

Currently features and architecture should be focused on creating an excellent foundation for further features and maintenance.

**The hook system is the foundation surface.** It's the in-tree precursor to the OpenClaw-style plugin architecture in the master plan. Reading order: `src/hooks/mod.rs` → `src/hooks/audit.rs` (reference handler) → `src/hooks/skills.rs` (built-in BeforePrompt) → how `src/agent.rs` wires the five events.

Five events: `BeforePrompt`, `BeforeToolCall`, `AfterToolCall`, `BeforeAgentEnd`, `session_start`/`session_end`. Outcomes: `Continue` / `Block` / `ReplaceArgs` / `ReplaceResult` / `InjectMessages { position }` / `ForceContinue`. First non-`Continue` wins for blocking events; injections accumulate.

Three load-bearing invariants:

1. **Long-lived Agent.** Hook handlers carry state (audit ring, future rate limits, approval caches). The TUI holds the Agent in `Arc<tokio::sync::Mutex<Agent>>` for the whole session — never construct a fresh Agent per prompt.
2. **`BeforePrompt` injections are transient.** `HookRegistry::apply_before_prompt` returns the outgoing wire payload; the persistent `messages` array is never mutated. Injecting a System message every turn is fine — it doesn't bloat saved history.
3. **Built-in vs. caller hooks.** `Agent::with_hooks` takes a `HookRegistry` by value, registers built-ins (currently `SkillsHook`), then `Arc`-wraps. Caller-supplied hooks (audit in TUI) are registered before the value is handed in.

**Skills** are no longer hard-coded into `PromptBuilder` — they flow through `SkillsHook` as a `BeforePrompt` injection at `InjectPosition::AfterSystem`. `PromptBuilder::build_system_prompt` is now tool-only.

**Tool dispatch** runs `BeforeToolCall` (block / replace args) → execute → `AfterToolCall` (replace result). Today `Tool::call` returns `Result<String>`; the master plan specifies `ToolResult { output, media, is_error }` — that upgrade is tracked in Linear and is the natural next structural change.

**Provider** is OpenAI-compatible (`src/provider/openai.rs`). Both buffered (`chat`) and streaming (`chat_stream`) paths are honored by every hook event; if you add a new event, wire it into both.

**Gateway daemon mode** is the Phase 2 foundation surface under `src/gateway/`. Reading order: `src/gateway/mod.rs` → `src/gateway/lane.rs` → `src/gateway/agent_map.rs` → `src/gateway/queue.rs` → `src/gateway/server.rs`. It owns Axum HTTP routes, durable SQLite inbound/outbound queues, per-lane long-lived agents with idle eviction, loopback test platform, and the Serenity-based Discord connector. Telegram and richer Discord controls build on the same `PlatformRegistry` and queue contracts.

## Active naming wart

The project was renamed from `ferris` → `vulcan`. Some references still say "ferris" (e.g. `config.example.toml` says `FERRIS_API_KEY` even though the code reads `VULCAN_API_KEY`; `~/wiki/queries/rust-hermes-plan.md` still says "ferris (binary)"). Fix in passing when you touch a file, but don't open a sweeping rename PR — the rename is being absorbed gradually.

<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **vulcan** (6948 symbols, 18155 relationships, 300 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

> If any GitNexus tool warns the index is stale, run `npx gitnexus analyze` in terminal first.

## Always Do

- **MUST run impact analysis before editing any symbol.** Before modifying a function, class, or method, run `gitnexus_impact({target: "symbolName", direction: "upstream"})` and report the blast radius (direct callers, affected processes, risk level) to the user.
- **MUST run `gitnexus_detect_changes()` before committing** to verify your changes only affect expected symbols and execution flows.
- **MUST warn the user** if impact analysis returns HIGH or CRITICAL risk before proceeding with edits.
- When exploring unfamiliar code, use `gitnexus_query({query: "concept"})` to find execution flows instead of grepping. It returns process-grouped results ranked by relevance.
- When you need full context on a specific symbol — callers, callees, which execution flows it participates in — use `gitnexus_context({name: "symbolName"})`.

## Never Do

- NEVER edit a function, class, or method without first running `gitnexus_impact` on it.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis.
- NEVER rename symbols with find-and-replace — use `gitnexus_rename` which understands the call graph.
- NEVER commit changes without running `gitnexus_detect_changes()` to check affected scope.

## Resources

| Resource | Use for |
|----------|---------|
| `gitnexus://repo/vulcan/context` | Codebase overview, check index freshness |
| `gitnexus://repo/vulcan/clusters` | All functional areas |
| `gitnexus://repo/vulcan/processes` | All execution flows |
| `gitnexus://repo/vulcan/process/{name}` | Step-by-step execution trace |

## CLI

| Task | Read this skill file |
|------|---------------------|
| Understand architecture / "How does X work?" | `.claude/skills/gitnexus/gitnexus-exploring/SKILL.md` |
| Blast radius / "What breaks if I change X?" | `.claude/skills/gitnexus/gitnexus-impact-analysis/SKILL.md` |
| Trace bugs / "Why is X failing?" | `.claude/skills/gitnexus/gitnexus-debugging/SKILL.md` |
| Rename / extract / split / refactor | `.claude/skills/gitnexus/gitnexus-refactoring/SKILL.md` |
| Tools, resources, schema reference | `.claude/skills/gitnexus/gitnexus-guide/SKILL.md` |
| Index, status, clean, wiki CLI commands | `.claude/skills/gitnexus/gitnexus-cli/SKILL.md` |

<!-- gitnexus:end -->

## Agent skills

### Backlog

GitHub Issues on `yycholla/vulcan` via `gh` CLI. See `docs/agents/backlog.md`.

### Triage labels

Canonical strings (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`); only `wontfix` exists today, others to be created. See `docs/agents/triage-labels.md`.

### Domain docs

Multi-context: `CONTEXT-MAP.md` + root `CONTEXT.md` + per-area `src/<area>/CONTEXT.md` (agent, daemon, gateway, hooks, provider, tools, tui). See `docs/agents/domain.md`.
