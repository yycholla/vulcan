# Recommendations

## Priority 1: Make The Wiki The Local Plan Root

The old `~/wiki/queries/rust-hermes-plan.md` reference was missing. Keep this repo-local `wiki/` folder as the working design/research home until a separate wiki repo is restored.

Next step:

- Add future cross-cutting agent research under `wiki/agents/`.
- Link implementation issues back to these files.

## Priority 2: Add A Visible Memory/Profile Contract

Vulcan already has session history, recall, and optional Cortex graph memory. The missing piece is a small, inspectable memory/profile layer like Hermes' bounded `MEMORY.md` and `USER.md`.

Smallest useful version:

- `vulcan memory show`
- `vulcan memory add/remove`
- `vulcan profile show/edit`
- bounded prompt injection at session start
- no autonomous writes without approval

Why:

- Gives users control.
- Keeps graph memory from becoming an invisible behavior source.
- Provides a safer base for future self-learning.

## Priority 3: Finish The Skill Draft Promotion Loop

Vulcan already has `auto_create_skills` draft behavior in the agent. Make it product-safe before expanding it.

Smallest useful version:

- generated skills land in `_pending/`
- command to list pending skills
- command to show source run/task
- command to promote or delete
- promoted skills become immutable unless explicitly edited

Skip:

- automatic self-improvement of installed skills
- remote skill hub
- skill marketplace

## Priority 4: Harden Persistent Actions With Provenance

Hermes' always-on memory + skills + cron + shell model highlights a real security issue: untrusted input can persist somewhere and fire later. Vulcan should make persistent actions carry provenance before gateway autonomy grows.

Apply to:

- memory writes
- skill writes/promotions
- scheduler jobs
- extension installs/enables
- file patches from gateway sessions

Smallest useful rule:

- record source surface, session, run id, user/channel, and canonical action digest
- require one-shot approval for persistent actions from untrusted or remote surfaces

## Priority 5: Improve Gateway Controls Before More Connectors

Do not chase Hermes' connector count. First, make the common gateway session controls crisp.

Useful commands:

- status
- stop
- approve/deny
- new/reset
- retry
- usage
- background
- resume

Add another connector only after the shared command model is stable.

## Priority 6: Add One Sandbox Backend Only If Needed

Hermes supports many execution backends. Vulcan should pick one only when a real workflow demands it.

Likely first choices:

- Docker backend for reproducible local isolation.
- SSH backend for running risky work away from the user's machine.

Do not add both in the same slice.

## Priority 7: Turn Provider Support Into A Capability Matrix

Mercury Coder is a provider/model candidate, not an agent. Vulcan should handle it through capability measurement.

Track per provider/model:

- streaming support
- tool-call support
- JSON schema behavior
- token usage availability
- context length
- pricing
- error shape
- rate-limit shape
- latency/throughput

Then Mercury can be added safely if it behaves well through the existing provider path.

## Priority 8: Extend Artifacts And Replay

Vulcan is stronger than Hermes here. Lean into it.

Next useful improvements:

- subagent transcript/artifact handle
- TUI timeline for child sessions
- artifact viewer commands
- replay/drift checks for read-only tools

These build on existing `RunRecord`, `ArtifactStore`, `ToolResult.details`, and `OrchestrationStore` instead of adding a new reporting system.
