# Hermes Agent

## Identity

Hermes Agent is an open-source Nous Research agent. Primary docs describe it as a self-improving autonomous agent with a built-in learning loop, memory, skills, gateway surfaces, scheduling, delegation, web/media tools, MCP, plugins, and sandboxed execution backends.

Primary sources:

- https://hermes-agent.nousresearch.com/
- https://hermes-agent.nousresearch.com/docs
- https://github.com/NousResearch/hermes-agent

## Feature Profile

Hermes is broad by design:

- CLI and desktop entry points.
- Messaging gateway with many platform adapters.
- Persistent memory and user profile files.
- Agent-created and agent-improved skills.
- 70+ tools across many toolsets.
- Web search, browser automation, vision, image generation, TTS.
- Cron jobs as first-class agent tasks.
- Subagent delegation.
- MCP support with filtering and catalog entries.
- Plugins from user, project, and entry point sources.
- ACP editor integration.
- Session storage with SQLite and FTS5.
- Trajectory export for training/research.

This breadth is useful for comparison, but most of it should not be copied directly into Vulcan.

## Architecture

Hermes centers on one `AIAgent` serving several entry points:

- CLI
- gateway
- ACP
- batch runner
- API server
- Python library

Major subsystems in official architecture docs:

- prompt builder
- runtime provider resolution
- tool dispatch and registry
- context compression/caching
- SQLite + FTS5 session storage
- tool backends
- gateway platform adapters
- plugin system
- cron
- ACP integration
- trajectories

Hermes design principles worth borrowing:

- platform-agnostic agent core
- observable execution
- interruptible API calls and tool execution
- loose coupling through registries and gating
- profile isolation

Vulcan already matches several of these through its daemon/session/runtime-pool split.

## Memory

Hermes uses bounded, curated memory:

- `MEMORY.md` for environment/project facts and lessons
- `USER.md` for user profile and preferences
- injected as a frozen snapshot at session start
- managed through a memory tool
- hard character limits force pruning instead of unbounded prompt growth

This is a useful product lesson. Vulcan has stronger graph-memory machinery, but Hermes has a simpler inspectable contract.

Borrow:

- small bounded visible memory ledgers
- explicit inspect/edit commands
- no silent auto-compaction of profile memory

Do not borrow yet:

- unconstrained autonomous memory mutation across surfaces

## Skills

Hermes skills are on-demand knowledge documents:

- compatible with `agentskills.io`
- live in `~/.hermes/skills`
- can be bundled, hub-installed, or agent-created
- loaded by slash command or natural conversation
- support progressive disclosure

Vulcan already has a skill registry and a pending auto-create flow. The useful gap is promotion workflow:

- draft generated skill
- inspect diff
- approve/promote
- track source task/run
- prevent silent mutation of trusted skills

## Tools And Sandboxes

Hermes has a much broader tool surface than Vulcan, including media and browser tooling. It also supports multiple terminal backends:

- local
- Docker
- SSH
- Singularity
- Modal
- Daytona

The lesson for Vulcan is not to add six backends. The useful first step is one explicit command-execution isolation story that fits the current trust profile model.

Likely first candidate:

- Docker backend for reproducible local sandboxes, if command isolation becomes urgent.

Alternative:

- SSH backend for running untrusted work away from the user's machine.

Do not add both before there is a real workflow that needs both.

## Gateway

Hermes' gateway supports many platforms and features like voice, images, files, threads, reactions, typing indicators, and streaming updates.

Vulcan should borrow the architecture idea, not the connector count:

- common platform event model
- per-chat session routing
- reset policies
- background session commands
- visible `/status`, `/stop`, `/approve`, `/deny` style controls

## Security

Hermes documents a seven-layer security model:

- user authorization
- dangerous command approval
- container isolation
- MCP credential filtering
- context-file prompt-injection scanning
- cross-session isolation
- input sanitization

The main warning for Vulcan: always-on agents with memory, skills, cron, shell, and messaging fold many authorities into one process. A recent arXiv paper calls out "sleeper channels": untrusted input persists into memory/skills/jobs/files and fires later through another surface. Vulcan's hook/audit/runtime-pool model is a good base for defending this, but persistent actions need stronger provenance gates.

## What Vulcan Should Learn

Borrow:

- visible memory/profile contract
- skill draft -> review -> promote lifecycle
- per-platform/toolset configuration UX
- gateway command vocabulary
- explicit sandbox backend story
- artifact/trajectory export mindset
- provenance gates for persistent actions

Avoid:

- copying 20 platform connectors
- adding many terminal backends before one is proven
- letting self-authored skills/memory/cron mutate without approval
- making plugin/runtime mechanisms overlap indefinitely
