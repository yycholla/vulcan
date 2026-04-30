# Vulcan

Vulcan is a pure-Rust personal AI agent whose frontends talk to a long-lived daemon that owns expensive runtime resources.

## Language

**Daemon**:
The long-lived Vulcan process that owns shared runtime resources and executes agent turns for all frontends.
_Avoid_: backend service, server process

**Frontend**:
A user- or platform-facing entrypoint, such as the TUI, CLI, gateway, or future connector, that talks to the **Daemon**.
_Avoid_: client UI, shell

**Runtime Resource Pool**:
The daemon-owned set of expensive adapters shared across sessions, including provider metadata, cortex memory, LSP processes, stores, and tool construction resources.
_Avoid_: full-stack session, per-session stack

**Session**:
A conversation-scoped agent state record with its own history, cancellation, in-flight turn state, and session-specific choices.
_Avoid_: lane, chat, process

**Provider Selection**:
The session-specific choice of provider profile and model used for future turns in that session.
_Avoid_: global model, daemon model

**Hook Instance**:
A session-local handler that observes, blocks, injects, or mutates one session's agent turns.
_Avoid_: global hook, shared hook state

**Tool Registry**:
A session-local set of tools exposed to the model after trust, profile, and workspace filtering.
_Avoid_: global tools, shared registry

**Turn**:
One ordered agent execution for a session, from user input through provider calls, tool calls, hooks, persistence, and final response.
_Avoid_: request, prompt call, job

**Turn Queue**:
An explicit per-session backlog of future turns accepted only when a frontend opts into queueing.
_Avoid_: implicit backlog, hidden queue

**Turn Runner**:
The session-local module that enforces single-flight execution and drives a turn state machine.
_Avoid_: prompt handler, stream handler, run path

**Turn Event**:
A domain-level event emitted by the **Turn Runner** and adapted into frontend streams, buffered responses, and run records.
_Avoid_: provider event, daemon frame

**Session History**:
The canonical in-memory transcript for a live session, durably mirrored to storage.
_Avoid_: database history, loaded messages

**Cortex Memory**:
The daemon-owned graph memory shared across sessions and scoped by metadata or query policy when needed.
_Avoid_: session cortex, memory database

**Daemon Client**:
A reusable frontend adapter that owns daemon socket communication and routes responses, stream frames, and daemon push frames by request id.
_Avoid_: one-shot socket, per-turn connection

**Storage Pool**:
The daemon-owned SQLite connection resource used by distinct storage adapters.
_Avoid_: per-agent database connection, module-owned connection

**Child Session**:
A session created by an active turn to run delegated agent work under parent-session lineage.
_Avoid_: subagent process, child agent

## Relationships

- Every **Frontend** talks to the **Daemon**; frontends do not construct an in-process agent fallback.
- The **Daemon** owns exactly one **Runtime Resource Pool** per running process.
- The **Runtime Resource Pool** eagerly owns correctness-critical global resources and lazily starts heavy optional adapters.
- A **Session** uses the **Runtime Resource Pool** but owns conversation-specific state.
- A **Session** owns its **Provider Selection**; provider metadata and catalog caches belong to the **Runtime Resource Pool**.
- A **Session** owns its **Hook Instances**; hook construction policy and heavy adapters belong to the **Runtime Resource Pool**.
- A **Session** owns its **Tool Registry**; tool factories and heavy adapters belong to the **Runtime Resource Pool**.
- A **Session** has at most one active **Turn**; different sessions can run turns concurrently.
- A **Turn Queue** is opt-in; a turn request for a busy **Session** is rejected unless the **Frontend** explicitly asks to queue it.
- A **Turn Runner** emits **Turn Events**; frontends and persistence adapt those events without changing turn execution.
- Cancellation targets the active **Turn**, not the **Session**.
- Parent-turn cancellation propagates to active turns in **Child Sessions**.
- A cancelled **Turn** preserves a partial transcript when work already happened, but **Session History** must remain valid for the next provider request.
- Compaction is **Turn Runner** behavior because it preserves provider context and tool-message invariants.
- A live **Session** owns canonical **Session History**; storage is the durability adapter and recovery source.
- The **Runtime Resource Pool** owns one **Cortex Memory** graph for the daemon process.
- **Cortex Memory** admin reads and edge maintenance go through the daemon-owned store, not transient second database opens.
- Session history, run records, and artifacts keep separate interfaces but share the daemon-owned **Storage Pool**.
- Delegated agent work runs in a **Child Session**, not by constructing an in-process child agent directly.
- A **Frontend** should reuse a **Daemon Client** when it has multiple daemon interactions in one process.
- A **Daemon Client** owns one read task per socket; normal responses, stream frames, and `id: null` daemon push frames are demultiplexed without stealing the socket.
- The **Daemon** keeps reading a connection while a streaming **Turn** is in flight; per-request dispatch runs independently and outbound frames are serialized by one writer queue.
- The gateway runtime owns its shared **Daemon Client** separately from lane/session routing; `DaemonLaneRouter` maps lanes to sessions and does not own daemon connection state.
- A gateway lane maps to one **Session**; the lane is a platform routing concept, not a daemon connection.

## Example Dialogue

> **Dev:** "Should `vulcan prompt` build an agent directly if the socket is unavailable?"
> **Domain expert:** "No. `vulcan prompt` is a **Frontend**; it auto-starts or connects to the **Daemon**, and the **Daemon** executes the turn through a **Session**."

## Flagged Ambiguities

- "client" can mean a frontend process, the daemon socket transport, or a provider HTTP client. Use **Frontend** for entrypoints and reserve "client" for concrete adapters.
- "session" and "lane" are related but distinct: a lane is gateway routing; a **Session** is agent conversation state.
