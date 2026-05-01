# Runtime Resource Pool Implementation Plan

## Context

This plan follows the daemon-required architecture and shared runtime resource pool decisions recorded in:

- `CONTEXT.md`
- `docs/adr/0001-daemon-required-frontends.md`
- `docs/adr/0002-shared-runtime-resource-pool.md`
- `docs/plans/2026-04-28-daemon-architecture-design.md`

The goal is to remove direct-mode assumptions from agent execution, make sessions the owner of conversation state, and let the daemon own expensive global adapters without changing user-visible behavior in one large step.

## Progress (2026-04-30)

- Slices 1-4 are implemented and verified in the current branch history.
- Slice 4 was deepened after testing: cortex prompt management now routes through daemon-owned cortex storage, and daemon cortex startup failures report the real open error instead of only `CORTEX_DISABLED`.
- Slice 5 is implemented: client-side request-id routing exists for normal responses, stream frames, and `id: null` push frames; daemon server handling continues reading the socket while stream requests are in flight and serializes outbound frames through one writer queue.
- Slice 6 is implemented: gateway runtime owns one reusable daemon client, workers and gateway slash commands share it, and `DaemonLaneRouter` is back to lane/session mapping only.
- Slice 7 is implemented for daemon-managed turns: `session.create` accepts child-session lineage metadata, `session.list` exposes parent session and lineage labels, and `spawn_subagent` now runs through daemon child sessions via a `SubagentRunner` seam.
- Direct child-agent construction fallback has been removed. If `spawn_subagent` is called before daemon session wiring installs a runner, it returns `SUBAGENT_REQUIRES_DAEMON`; launch paths are expected to auto-start/connect to the daemon before prompts run.
- Remaining Slice 7 hardening: exercise a full daemon prompt that calls `spawn_subagent` once provider-backed integration fixtures are available.

## Slice 1: Turn Runner Seam

Introduce a session-local `TurnRunner` and domain-level `TurnEvent`.

- Keep `Agent` as the product/domain facade.
- Move buffered and streaming turn execution behind one state machine.
- Make `prompt.run` and `prompt.stream` adapters over the same runner.
- Preserve current behavior for hooks, tool dispatch, run records, compaction, cancellation, and persistence.
- Add tests proving buffered and streaming paths share semantics for tool calls, finalization, cancellation, and max-iteration behavior.

Acceptance:

- No duplicated turn loop between buffered and streaming execution.
- `TurnEvent` is the internal interface; daemon frames and buffered text are adapters.
- Existing CLI/TUI/gateway behavior remains compatible.

## Slice 2: Session History Ownership

Move canonical live history from per-turn SQLite reloads into session-owned in-memory state.

- Add session-owned `SessionHistory`.
- Load and heal persisted history once when a session is created or resumed.
- Turn Runner mutates the in-memory snapshot.
- History adapter durably persists appends and atomic replacements.
- Remove full `load_history` from the hot path at the start of every turn.

Acceptance:

- A live session has canonical in-memory `SessionHistory`.
- Storage is durability and recovery, not the hot-path source of truth.
- Compaction updates in-memory and durable history through one adapter.
- Cancelled turns preserve valid partial transcript state.

## Slice 3: Runtime Resource Pool Extraction

Introduce `RuntimeResourcePool` as the daemon-owned holder of expensive/global adapters.

- Move storage pool, provider catalog/cache infrastructure, LSP pool, cortex memory, and tool/hook factories into the pool.
- Replace session lazy-build calls to all-in-one `Agent::builder(config).build()` with session assembly from pool adapters.
- Keep session-local provider selection, hook instances, tool registry, cancellation, turn state, and history.
- Keep store interfaces separate while sharing daemon-owned storage resources where appropriate.

Acceptance:

- New sessions do not rebuild full-stack runtime resources.
- Hook instances and tool registries are session-local but assembled from daemon-owned factories/adapters.
- Provider/model selection remains session-specific while catalog/cache infrastructure is shared.

## Slice 4: Cortex Admin and Storage Fix

Make cortex admin operations use daemon-owned storage instead of transient second redb opens.

- Remove normal daemon usage of transient `RedbStorage::open`.
- Add a daemon-owned cortex admin/storage seam.
- Make `cortex.stats` avoid per-node graph traversal.
- Ensure `edges_from`, `edges_to`, `delete_edge`, and `run_decay` work while the daemon is running.

Acceptance:

- `cortex.stats` is not O(N) traversals.
- Cortex edge/admin operations do not fail because the daemon owns the redb lock.
- Session scoping, if needed, is metadata/query policy over one daemon-owned cortex graph.

## Slice 5: Client Transport Multiplexing

Move daemon socket transport from one-call ownership to request-id multiplexing.

- Add one socket read task per connection.
- Route final responses and stream frames by request id.
- Support `id: null` daemon push frames.
- Keep `Client` as the frontend-facing adapter.

Acceptance:

- A single `DaemonClient` can handle multiple in-flight daemon interactions.
- Streaming calls and normal calls share request-id routing.
- The transport no longer has to steal and replace its UnixStream for streaming calls.

## Slice 6: Gateway Shared Daemon Client

Make the gateway reuse a daemon client instead of opening a fresh connection per inbound row.

- `GatewayState` owns one reusable `DaemonClient`.
- `DaemonLaneRouter` remains responsible for lane-to-session mapping only.
- Worker turns flow through the shared client.
- Reconnect behavior belongs to the client adapter, not per-worker code.

Acceptance:

- Gateway lane routing is independent from daemon connection ownership.
- Workers do not open a fresh daemon socket for every row.
- Connection pooling is deferred until load testing proves one multiplexed socket is a bottleneck.

## Slice 7: Child Sessions for Subagents

Move delegated agent work from direct child `Agent` construction to daemon child sessions.

- Parent turn creates a `Child Session` with parent-session lineage.
- Child session gets its own provider selection, history, hook instances, and filtered tool registry.
- Parent cancellation propagates to active child turns.
- Child summaries, run records, and artifacts link back to the parent run.
- Completed child sessions may be evicted unless explicitly kept.

Acceptance:

- `spawn_subagent` does not build a direct in-process child agent.
- Child work uses the same daemon/session/turn semantics as frontend work.
- Parent-child lineage is visible through run records and session metadata.

## Notes

- This plan intentionally sequences execution and history seams before resource pooling, so later pooling work does not preserve duplicated turn-loop assumptions.
- This plan intentionally sequences cortex fixes before transport multiplexing because cortex has an active correctness/performance issue, while one-call transport is primarily a scalability limitation.
