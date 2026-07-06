# Mercury Agent

## Identity

Primary repo: https://github.com/cosmicstack-labs/mercury-agent

Mercury Agent is a TypeScript/Node personal AI agent from Cosmic Stack. The repo describes it as a soul-driven agent with permission-hardened tools, token budgets, multi-channel access, 31 built-in tools, Kanban boards, extensible skills, and SQLite-backed Second Brain memory.

Current stable in the repo README/package metadata: `v1.1.13`.

License: MIT.

This is separate from Inception Labs' Mercury Coder model. Mercury Agent is an agent framework; Mercury Coder is still relevant to Vulcan only as a provider/model candidate.

## Architecture

Mercury's architecture is channel-first and Node ecosystem oriented:

- TypeScript + Node.js runtime.
- Vercel AI SDK for `generateText` / `streamText` and provider fallback.
- Ink + React for CLI/TUI.
- grammY for Telegram.
- Hono/React web dashboard.
- SQLite + FTS5 for Second Brain memory.
- JSONL/JSON for short-term, long-term, and episodic memory.
- Node cron scheduler.
- Daemon manager with PID file, watchdog, and system service integration.

Core source areas from `ARCHITECTURE.md`:

- `src/core/agent.ts` - multi-step agent loop.
- `src/channels/` - CLI, Telegram, and channel registry.
- `src/capabilities/` - tools and permission manager.
- `src/memory/` - memory store and Second Brain database.
- `src/providers/` - model providers.
- `src/skills/` - Agent Skills spec loader.
- `src/core/scheduler.ts` - cron and heartbeat.
- `src/core/sub-agent.ts` / `src/core/supervisor.ts` - subagents.
- `src/core/file-lock.ts` - file coordination.
- `src/core/task-board.ts` - shared task state.

## Agent Loop

Mercury uses Vercel AI SDK multi-step generation with tools:

1. Load system prompt from soul/persona/guardrails.
2. Call the model with tools and max steps.
3. If a tool is called, check permissions first.
4. Execute or return a denial to the LLM.
5. Continue until text response or max steps.
6. Send response through the active channel.

Compared with Vulcan, this is less daemon/session-pool oriented, but more complete as an end-user product loop.

## Permissions

Mercury's most useful idea for Vulcan is its simple permission UX:

- folder-level read/write scoping
- per-session `Ask Me` or `Allow All`
- one-time or persistent scope grants
- shell blocklist for dangerous commands
- auto-approved safe commands
- explicit approval for risky commands
- scope manifest at `~/.mercury/permissions.yaml`

Vulcan already has trust profiles, approval hooks, replay safety, and tool capability profiles. Mercury's lesson is product shape: make permission state visible and editable to the user.

## Memory

Mercury's Second Brain is a structured long-term user model:

- SQLite + FTS5.
- 10 memory types: identity, preference, goal, project, habit, decision, constraint, relationship, episode, reflection.
- background extraction after conversations.
- confidence, importance, durability scores.
- top 5 relevant memories injected into context under a 900-character budget.
- auto-consolidation and pruning.
- conflict resolution by confidence/recency.
- `/memory` controls for overview, search, pause/resume, and clear.

Important caveat: the architecture docs say there is no review queue or manual memory editing. Vulcan should not copy that part until persistent-action provenance exists.

## Skills

Mercury adopts the Agent Skills specification:

- skills live under `~/.mercury/skills/`
- `SKILL.md` with YAML frontmatter
- progressive disclosure: names/descriptions at startup, full instructions on invocation
- install/list/use skill tools
- registry at `skills.mercuryagent.sh`
- installable via CLI, Web, or Telegram

This overlaps strongly with Vulcan's skills direction. The useful Mercury lesson is user-facing skill management across channels.

## Scheduler And Daemon

Mercury has first-class daemon commands:

- `mercury up`
- `restart`
- `stop`
- `logs`
- `status`
- `service install/status/uninstall`

System service targets:

- macOS LaunchAgent
- Linux systemd user unit
- Windows Task Scheduler

Scheduling:

- recurring cron tasks
- one-shot delayed tasks
- persisted in `~/.mercury/schedules.yaml`
- route responses back to the creating channel

Vulcan has daemon/gateway/scheduler foundations. Mercury's lesson is CLI ergonomics and operational packaging.

## Subagents, File Locks, And Boards

Mercury subagents run as async workers in the same Node process:

- resource-aware max concurrency
- background progress notifications
- user controls to list, stop, pause, resume, and halt agents
- file locks for read/write coordination
- task board persisted to disk
- Plan and Execute programming modes

Vulcan already has daemon child sessions and orchestration state. Borrow:

- file lock semantics for concurrent coding agents
- visible subagent commands and timeline
- task-board/artifact linkage
- Plan/Execute mode as a product surface over existing run records and Symphony/workflows

Avoid:

- duplicating Mercury's task board if Vulcan artifacts/Symphony can cover the same job.

## Web Dashboard

Mercury includes a localhost web UI:

- SSE chat streaming
- Kanban boards
- Second Brain visualization
- Workspace IDE
- provider/skill/permission/schedule management
- usage tracking

Vulcan should not rush a dashboard, but the management surfaces are relevant. If Vulcan adds web UI later, it should manage existing daemon/session/artifact state rather than create a parallel app model.

## Provider Layer

Mercury uses the Vercel AI SDK and provider fallback:

- DeepSeek
- OpenAI
- Anthropic
- Grok/xAI
- Ollama Cloud
- Ollama Local
- custom OpenAI-compatible endpoints

It remembers the last successful provider and starts there next time.

Vulcan takeaway: keep provider fallback/capability checks explicit. Do not replace Vulcan's provider layer with a JS-style SDK concept, but do borrow the user-facing fallback model.

## What Vulcan Should Learn

Borrow:

- visible permission manifest and simple approval modes
- hard daily token budget with commands
- Second Brain controls, but with review/provenance before autonomous writes
- service management ergonomics
- subagent file-lock semantics
- Plan/Execute mode as a thin product state
- skill registry management surfaces
- provider fallback UX

Do not borrow:

- autonomous memory mutation without review controls
- a separate Kanban database if artifacts/Symphony can represent tasks
- Node/Vercel AI SDK architecture
- same-process subagents if daemon child sessions already solve isolation better
