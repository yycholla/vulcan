# Comparison

## Short Matrix

| Area                   | Vulcan                                                                         | Hermes Agent                                                        | Mercury Agent                                                      |
| ---------------------- | ------------------------------------------------------------------------------ | ------------------------------------------------------------------- | ------------------------------------------------------------------ |
| Category               | Rust local agent runtime                                                       | broad autonomous agent platform                                     | TypeScript personal agent                                          |
| Core architecture      | daemon-owned runtime pool, session-local agents                                | one AIAgent serving CLI/gateway/ACP/batch/API                       | channel-first Node agent with Vercel AI SDK                        |
| Frontends              | CLI, TUI, daemon client, gateway foundation                                    | CLI, desktop, gateway, ACP/editor, API                              | CLI Ink TUI, Telegram, Web dashboard                               |
| Gateway/channels       | Discord, Telegram, loopback foundation                                         | 20+ messaging platforms                                             | CLI, Telegram, Web; future Signal/Discord/Slack named              |
| Tools                  | structured tool trait, replay safety, MCP adapter, code/file/shell/LSP/tools   | 70+ tools and toolsets                                              | 31 built-in tools across files, shell, git, web, skills, scheduler |
| Tool results           | structured `ToolResult` with details/media/errors/previews                     | broad tool registry, tool backends                                  | Vercel AI SDK tool results                                         |
| Memory                 | session store, recall, optional Cortex graph memory                            | bounded `MEMORY.md` and `USER.md`, session search, Honcho provider  | SQLite + FTS5 Second Brain, JSONL memories                         |
| Skills                 | registry plus pending auto-create flow                                         | first-class, agent-created, hub/installable, progressive disclosure | Agent Skills spec, registry install, CLI/Web/Telegram management   |
| Subagents              | bounded daemon child sessions via `spawn_subagent`                             | isolated subagents with conversations/terminals/RPC scripts         | same-process async subagents with supervisor/file locks            |
| Scheduling             | scheduler/gateway foundation                                                   | first-class cron agent tasks                                        | cron and one-shot scheduled tasks                                  |
| Sandboxing/permissions | file/tool policies, trust profile, MCP controls; command backend less explicit | local/Docker/SSH/Singularity/Modal/Daytona                          | folder scopes, shell blocklist, Ask Me/Allow All modes             |
| Plugins/extensions     | native Cargo extension architecture, frontend/daemon split, state store        | plugin manager, tools/hooks/commands, memory/context plugins        | markdown skills rather than code plugins                           |
| Provider routing       | OpenAI-compatible provider path and catalog                                    | 18+ providers and three API modes                                   | Vercel AI SDK provider fallback                                    |
| Work management        | run records, artifacts, replay foundations, Symphony                           | trajectories and batch runner                                       | Kanban boards, task cards, Plan/Execute programming mode           |

## Vulcan Strengths

- Cleaner Rust-native core.
- Strong daemon/session/resource-pool split.
- Structured tool result contract is already in place.
- Run records and artifacts give Vulcan a stronger audit/replay foundation.
- Child-session subagents are already bounded and cancellable.
- Extension frontend/daemon split is more disciplined than a single global plugin bucket.
- MCP exists and is opt-in by default.

## Hermes Strengths

- Broader product surface.
- Simpler visible memory contract.
- Stronger skill lifecycle and user-facing skill affordances.
- More mature gateway UX vocabulary.
- Rich tool and media surface.
- Explicit sandbox backend options.
- More provider routing breadth.
- Documentation is split into user/developer/reference pages and has LLM-readable indexes.

## Mercury Strengths

- Strong permission UX: folder scopes, shell blocklist, session modes, persistent grants.
- Hard token budget commands.
- More complete end-user daemon/service commands.
- Second Brain memory product surface.
- Kanban boards and visible task execution.
- Plan/Execute programming mode.
- File locks for same-workspace subagents.
- Provider fallback UX.

## Gaps To Close

1. Documentation gap: local master plan path was missing; current wiki should become the working local plan root.
2. Memory UX gap: Vulcan has powerful storage, but not Hermes/Mercury-style visible memory/profile controls.
3. Skill lifecycle gap: Vulcan can draft skills, but needs review/promote/trace semantics before letting the agent self-modify skills.
4. Permission UX gap: Vulcan has trust/profile primitives, but Mercury makes permission state easier to inspect and change.
5. Gateway UX gap: Vulcan should define common gateway controls before adding many connectors.
6. Sandbox policy gap: Vulcan needs one explicit command execution isolation path if it wants Hermes-level remote/gateway autonomy.
7. Persistent-action provenance gap: memory writes, skill writes, cron jobs, and filesystem patches should carry approval/provenance when sourced from untrusted channels.
8. Token budget UX gap: Vulcan tracks usage, but Mercury makes budget state and overrides an explicit command surface.
9. Work-board gap: Vulcan has artifacts/run records/Symphony, but Mercury has user-visible boards and card execution.
10. Provider fallback gap: Mercury exposes fallback as a product feature; Vulcan should make this explicit if multiple providers are configured.

## Things Not Worth Copying Now

- Hermes' full connector count.
- Six terminal backends.
- Autonomous skill rewriting without a review gate.
- Mercury's autonomous memory mutation without review controls.
- Mercury's Node/Vercel AI SDK architecture.
- A separate Kanban database if artifacts/Symphony can represent tasks.
