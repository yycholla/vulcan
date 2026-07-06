# CLAUDE.md

Guidance for Claude Code (claude.ai/code) when work with code in this repo.

Vulcan = pure-Rust personal AI agent (CLI + TUI). Binary `vulcan`; chat = default subcommand.

## Version control

Repo **jj (Jujutsu) colocated** — `jj root` succeeds. Use jj, not git, for changes. Git read commands fine; make commits/history via jj. Load `jj-vcs` skill before writing commit message or touching history — holds required workflow + commit-message format policy; don't rely on general jj knowledge.

## Github PR flow

- Plain GitHub flow via `gh` CLI. One branch per GitHub issue; open PRs against `main`. Use issue-closure keywords (`Closes #N`) in PR body for automation.

## Where work is tracked and planned

- **GitHub Issues** = source of truth for tasks in `yycholla/vulcan`. Historical `YYC-` identifiers carried over in issue titles where useful. Check existing issues before creating new; ask before bulk-creating tickets. Group related work with labels/milestones where possible.
- **`~/wiki/queries/rust-hermes-plan.md`** = master vision (Phase 1 → 2 → 3, locked Phase 1 scope, tool trait + provider trait shapes). Anything contradicting plan = open question or doc lag — don't silently diverge.
- **`~/wiki/queries/`** also holds design docs (e.g. `hooks-design.md` once written) — prefer adding cross-cutting design docs there, not in repo.

## Build / run / test

```bash
cargo build --all-targets        # compile everything, including tests
cargo run                         # launch TUI (default subcommand: chat)
cargo run -- prompt "your text"   # one-shot mode (no TUI)
cargo run -- session <id>         # resume a saved session
cargo run --features gateway -- gateway  # run gateway daemon mode
cargo build --release             # size-optimized binary (opt-level=z, lto, strip)
cargo test                        # run tests
cargo test --features gateway gateway::  # gateway feature tests
cargo test <name_substring>       # single test by name
```

Logging: TUI mode logs to file (so `tracing` output no splat screen); one-shot mode logs to stderr. Set `RUST_LOG=debug` for verbose.

Config: `~/.vulcan/config.toml` (see `config.example.toml`). API key via `VULCAN_API_KEY` env var or config file.

## Architecture worth reading multiple files to understand

Currently features + architecture should focus on making excellent foundation for further features + maintenance.

**Hook system = foundation surface.** In-tree precursor to OpenClaw-style plugin architecture in master plan. Reading order: `src/hooks/mod.rs` → `src/hooks/audit.rs` (reference handler) → `src/hooks/skills.rs` (built-in BeforePrompt) → how `src/agent.rs` wires the five events.

Five events: `BeforePrompt`, `BeforeToolCall`, `AfterToolCall`, `BeforeAgentEnd`, `session_start`/`session_end`. Outcomes: `Continue` / `Block` / `ReplaceArgs` / `ReplaceResult` / `InjectMessages { position }` / `ForceContinue`. First non-`Continue` wins for blocking events; injections accumulate.

Three load-bearing invariants:

1. **Long-lived Agent.** Hook handlers carry state (audit ring, future rate limits, approval caches). TUI holds Agent in `Arc<tokio::sync::Mutex<Agent>>` whole session — never construct fresh Agent per prompt.
2. **`BeforePrompt` injections transient.** `HookRegistry::apply_before_prompt` returns outgoing wire payload; persistent `messages` array never mutated. Injecting System message every turn fine — no bloat saved history.
3. **Built-in vs. caller hooks.** `Agent::with_hooks` takes `HookRegistry` by value, registers built-ins (currently `SkillsHook`), then `Arc`-wraps. Caller-supplied hooks (audit in TUI) registered before value handed in.

**Skills** no longer hard-coded into `PromptBuilder` — flow through `SkillsHook` as `BeforePrompt` injection at `InjectPosition::AfterSystem`. `PromptBuilder::build_system_prompt` now tool-only.

**Tool dispatch** runs `BeforeToolCall` (block / replace args) → execute → `AfterToolCall` (replace result). Today `Tool::call` returns `Result<String>`; master plan specifies `ToolResult { output, media, is_error }` — that upgrade tracked in GitHub Issues, = natural next structural change.

**Provider** = OpenAI-compatible (`src/provider/openai.rs`). Both buffered (`chat`) + streaming (`chat_stream`) paths honored by every hook event; if add new event, wire into both.

**Gateway daemon mode** = Phase 2 foundation surface under `src/gateway/`. Reading order: `src/gateway/mod.rs` → `src/gateway/lane.rs` → `src/gateway/agent_map.rs` → `src/gateway/queue.rs` → `src/gateway/server.rs`. Owns Axum HTTP routes, durable SQLite inbound/outbound queues, per-lane long-lived agents with idle eviction, loopback test platform, Serenity-based Discord connector. Telegram + richer Discord controls build on same `PlatformRegistry` + queue contracts.

## Active naming wart

Project renamed `ferris` → `vulcan`. Some references still say "ferris" (e.g. `config.example.toml` says `FERRIS_API_KEY` even though code reads `VULCAN_API_KEY`; `~/wiki/queries/rust-hermes-plan.md` still says "ferris (binary)"). Fix in passing when touch a file, but don't open sweeping rename PR — rename absorbed gradually.

<!-- gitnexus:start -->

# GitNexus — Code Intelligence

Project indexed by GitNexus as **vulcan** (10654 symbols, 26938 relationships, 300 execution flows). Use GitNexus MCP tools to understand code, assess impact, navigate safely.

> If any GitNexus tool warns index stale, run `npx gitnexus analyze` in terminal first.

## Always Do

- **MUST run impact analysis before editing any symbol.** Before modifying function, class, or method, run `gitnexus_impact({target: "symbolName", direction: "upstream"})` + report blast radius (direct callers, affected processes, risk level) to user.
- **MUST run `gitnexus_detect_changes()` before committing** to verify changes only affect expected symbols + execution flows.
- **MUST warn user** if impact analysis returns HIGH or CRITICAL risk before proceeding with edits.
- When exploring unfamiliar code, use `gitnexus_query({query: "concept"})` to find execution flows instead of grepping. Returns process-grouped results ranked by relevance.
- When need full context on specific symbol — callers, callees, which execution flows it joins — use `gitnexus_context({name: "symbolName"})`.

## Never Do

- NEVER edit function, class, or method without first running `gitnexus_impact` on it.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis.
- NEVER rename symbols with find-and-replace — use `gitnexus_rename` which understands call graph.
- NEVER commit changes without running `gitnexus_detect_changes()` to check affected scope.

## Resources

| Resource                                | Use for                                  |
| --------------------------------------- | ---------------------------------------- |
| `gitnexus://repo/vulcan/context`        | Codebase overview, check index freshness |
| `gitnexus://repo/vulcan/clusters`       | All functional areas                     |
| `gitnexus://repo/vulcan/processes`      | All execution flows                      |
| `gitnexus://repo/vulcan/process/{name}` | Step-by-step execution trace             |

## CLI

| Task                                         | Read this skill file                                        |
| -------------------------------------------- | ----------------------------------------------------------- |
| Understand architecture / "How does X work?" | `.claude/skills/gitnexus/gitnexus-exploring/SKILL.md`       |
| Blast radius / "What breaks if I change X?"  | `.claude/skills/gitnexus/gitnexus-impact-analysis/SKILL.md` |
| Trace bugs / "Why is X failing?"             | `.claude/skills/gitnexus/gitnexus-debugging/SKILL.md`       |
| Rename / extract / split / refactor          | `.claude/skills/gitnexus/gitnexus-refactoring/SKILL.md`     |
| Tools, resources, schema reference           | `.claude/skills/gitnexus/gitnexus-guide/SKILL.md`           |
| Index, status, clean, wiki CLI commands      | `.claude/skills/gitnexus/gitnexus-cli/SKILL.md`             |

<!-- gitnexus:end -->

## Agent skills

### Backlog

GitHub Issues on `yycholla/vulcan` via `gh` CLI. See `docs/agents/backlog.md`.

### Triage labels

Canonical strings (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`); only `wontfix` exists today, others to be created. See `docs/agents/triage-labels.md`.

### Domain docs

Multi-context: `CONTEXT-MAP.md` + root `CONTEXT.md` + per-area `src/<area>/CONTEXT.md` (agent, daemon, gateway, hooks, provider, tools, tui). See `docs/agents/domain.md`.

# SlayZone Environment

You = agent running inside [SlayZone](https://slayzone.com) task. Other agents may run in own tasks in parallel, and human or another agent can reach you through this terminal any time.

## Interact with SlayZone

If useful, you have toolbox for acting on SlayZone itself. You can:

- create + update tasks, and spawn sub-tasks with own agents
- attach assets, run processes, open web panels, set up automations
- change own task's state

Toolbox = `slay` CLI. `$SLAYZONE_TASK_ID` holds your task's ID, most `slay` commands default to it. **Load `slay` skill before running any `slay` command** — holds full reference of commands, flags, domain-specific guides. Never guess subcommands or flags.
