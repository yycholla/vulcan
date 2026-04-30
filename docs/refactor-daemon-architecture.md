# YYC-266: Daemon Architecture — Thin Frontends, Shared Backend

## Status: Proposal

## Problem

Every `vulcan` invocation starts from scratch. There is no daemon, no background
process, no shared state between invocations. Each call to `Agent::builder().build()`
pays the full cold-start cost, and the cortex.redb exclusive file lock prevents
concurrent access from CLI while the TUI is running.

### Current Architecture

```
┌───────────────────────────────────────────────┐
│  vulcan (single process, cold start every time)│
│                                               │
│  Entry point         What it builds           │
│  ──────────          ──────────────           │
│  vulcan / chat       Full Agent               │
│  vulcan prompt       Full Agent               │
│  vulcan cortex       CortexStore + Session    │
│  vulcan gateway      Full Agent per lane       │
│  vulcan search       SessionStore only        │
└───────────────────────────────────────────────┘
```

### What `Agent::build_from_parts()` Does Every Time

1. **Network call** — fetch provider `/v1/models` catalog
2. **Provider init** — HTTP client, auth headers, base URL
3. **Tool registry** — instantiate ~16 tools, LSP manager, workspace probe
4. **Hook registry** — 10 hooks including cortex store open + embedding model load + HNSW index rebuild
5. **Session store** — new SQLite connection (FTS5)
6. **Orchestration store** — new in-memory store
7. **Run record store** — another SQLite connection
8. **Artifact store** — another SQLite connection
9. **Cortex store** (if enabled) — open redb, load embedding model, rebuild HNSW from all stored nodes

### Concrete Pain Points

| Symptom | Root cause |
|---|---|
| `vulcan cortex search` fails while TUI is running | redb exclusive `flock` on cortex.redb — two processes can't open it |
| `vulcan prompt "..."` takes 3-5 seconds before first token | Full `Agent::build_from_parts()` + network catalog fetch |
| `vulcan cortex stats` loads embedding model for 1-2 seconds just to count nodes | `CortexStore::try_open()` always loads the model + rebuilds HNSW |
| `vulcan search` hits cold SQLite every time | No shared connection pool |
| LSP servers start from scratch every invocation | No persistent LSP pool |
| Gateway builds one Agent per lane, no sharing | Each lane = full cold start |

### The redb Lock (In Detail)

redb uses `flock(LOCK_EX | LOCK_NB)` via `libc::flock()` on the database file in
`FileBackend::new()` (see `redb-2.6.3/src/tree_store/page_store/file_backend/unix.rs`).
This is an exclusive, non-blocking advisory lock tied to the open-file-description.

- **Cross-process** (CLI vs TUI): second `open()` → `flock(LOCK_EX | LOCK_NB)` → `EWOULDBLOCK` → `DatabaseAlreadyOpen`.
- **Same-process** (transient handle inside TUI): a second `open()` creates a different file description; `flock` treats them independently, so the second `LOCK_EX` also fails.

The current workaround — transient `RedbStorage` handles in `CortexStore::edges_from()`,
`edges_to()`, `delete_edge()`, `update_edge_weight_atomic()`, `run_decay()` — only
works when no other process holds the DB. These methods are limited to "CLI only,
when TUI isn't running."

The `stats()` method works around the lock by using `traverse(depth=1)` for each
node to count edges — O(N) traversals, slow for large graphs.

---

## Proposal: Daemon + Thin Frontends

### Target Architecture

```
┌─────────────────────────────────────────────┐
│  vulcan daemon (long-lived process)          │
│                                             │
│  ┌───────────┐  ┌──────────┐  ┌─────────┐  │
│  │  Agent    │  │ Cortex   │  │ Session │  │
│  │  (warm)   │  │ Store    │  │ Store   │  │
│  └───────────┘  └──────────┘  └─────────┘  │
│  ┌───────────┐  ┌──────────┐  ┌─────────┐  │
│  │ LSP pool  │  │ Tool     │  │ Hook    │  │
│  │           │  │ Registry │  │ Registry│  │
│  └───────────┘  └──────────┘  └─────────┘  │
│  ┌───────────┐  ┌──────────┐               │
│  │ Artifact  │  │ Run      │               │
│  │ Store     │  │ Records  │               │
│  └───────────┘  └──────────┘               │
│                                             │
│  Unix socket: ~/.vulcan/vulcan.sock         │
└──────────────────┬──────────────────────────┘
                   │
          ┌────────┼───────────┐
          │        │           │
     ┌────┴───┐ ┌──┴────┐ ┌───┴──────┐
     │  TUI   │ │  CLI  │ │ Gateway  │
     │ (thin) │ │ (thin)│ │  (thin)  │
     └────────┘ └───────┘ └──────────┘
```

### What "Thin" Means

- **TUI**: sends user input → daemon → renders `StreamEvent`s back. No longer
  holds `Arc<Mutex<Agent>>` directly; consumes an async event stream from the
  daemon. Model switching, session resume, tool approval all become RPCs.
- **CLI** (`vulcan prompt`, `vulcan cortex`, `vulcan search`): serializes request
  → sends over socket → receives response → prints. Falls back to direct open
  when daemon is not running (for offline / one-shot use).
- **Gateway**: already a thin HTTP frontend over agents — just talks to the
  daemon instead of building its own `Agent` per lane.

### What the Daemon Holds

All the expensive-to-build, long-lived state:

| Resource | Lifetime | Notes |
|---|---|---|
| `Agent` | Daemon lifetime | Provider client stays warm, no re-auth |
| `CortexStore` | Daemon lifetime | redb lock held by one process, no conflicts |
| Embedding model | Daemon lifetime | fastembed model stays loaded (1-2s savings per call) |
| HNSW index | Daemon lifetime | No rebuild on every `cortex search` |
| `SessionStore` | Daemon lifetime | SQLite connection pool stays warm |
| LSP server pool | Daemon lifetime | Servers stay running, no cold start |
| `ToolRegistry` | Daemon lifetime | All 16 tools stay instantiated |
| `HookRegistry` | Daemon lifetime | All 10 hooks stay registered |
| `ArtifactStore` | Daemon lifetime | SQLite connection warm |
| `RunStore` | Daemon lifetime | SQLite connection warm |

---

## Unix Domain Socket Protocol

### Socket Location

```
~/.vulcan/vulcan.sock
```

Permissions: `0600` (owner read/write only). The daemon creates it on startup
and removes it on clean shutdown. Stale socket detection: `connect()` fails with
`ECONNREFUSED` → remove and start daemon (or fall back to direct mode).

### Wire Format

Length-delimited JSON frames over `UnixStream`:

```
┌──────────────┬──────────────────────────────┐
│  u32 BE len  │  JSON body (Request | Response) │
└──────────────┴──────────────────────────────┘
```

JSON for simplicity and debuggability. Can switch to bincode later if throughput
matters — the protocol is versioned so this is a wire-format change, not an API
change.

### Protocol Version

Every request includes `"version": 1`. The daemon rejects mismatched versions
with an error response. This lets us evolve the protocol without breaking old
clients during rolling upgrades.

### Request Envelope

```json
{
  "version": 1,
  "id": "uuid-v4",
  "method": "prompt.run",
  "params": { ... }
}
```

### Response Envelope

```json
{
  "version": 1,
  "id": "uuid-v4",
  "result": { ... },
  "error": null
}
```

Or on error:

```json
{
  "version": 1,
  "id": "uuid-v4",
  "result": null,
  "error": {
    "code": "CORTEX_LOCKED",
    "message": "cortex.redb is locked by another process"
  }
}
```

### Streaming

For `prompt.run` and `prompt.stream`, the daemon sends multiple response frames
with the same `id`:

```
Client → Daemon:  { "id": "1", "method": "prompt.stream", "params": { "text": "..." } }
Daemon → Client:  { "id": "1", "stream": "text", "data": { "chunk": "Hello" } }
Daemon → Client:  { "id": "1", "stream": "text", "data": { "chunk": " world" } }
Daemon → Client:  { "id": "1", "stream": "done", "data": { "usage": { ... } } }
```

The `"stream"` field differentiates intermediate frames from the final frame
(`"done"`). The TUI consumes these incrementally for live rendering. The CLI
buffers them and prints the concatenated text.

### Method Catalog

#### Agent Operations

| Method | Params | Response | Notes |
|---|---|---|---|
| `agent.status` | `{}` | `{ model, session_id, turns }` | Quick health check |
| `agent.switch_model` | `{ model }` | `{ model, max_context }` | Hot-swap provider |
| `agent.list_models` | `{}` | `{ models: [...] }` | Provider catalog |

#### Prompt Execution

| Method | Params | Response | Notes |
|---|---|---|---|
| `prompt.run` | `{ text, continue? }` | Stream: `text`, `tool_call`, `done` | Full agent loop with tools |
| `prompt.cancel` | `{}` | `{ ok: true }` | Cancel in-flight turn |

#### Cortex Operations

| Method | Params | Response | Notes |
|---|---|---|---|
| `cortex.store` | `{ text, importance }` | `{ node_id }` | Store a fact |
| `cortex.search` | `{ query, limit }` | `{ results: [...] }` | Semantic search |
| `cortex.stats` | `{}` | `{ nodes, edges, db_size }` | Graph statistics |
| `cortex.recall` | `{ limit }` | `{ nodes: [...] }` | Recent memory |
| `cortex.seed` | `{ sessions }` | `{ stored }` | Seed from sessions |
| `cortex.edges_from` | `{ node_id }` | `{ edges: [...] }` | Edge listing |
| `cortex.edges_to` | `{ node_id }` | `{ edges: [...] }` | Edge listing |
| `cortex.delete_edge` | `{ edge_id }` | `{ ok: true }` | Edge deletion |
| `cortex.run_decay` | `{}` | `{ pruned, deleted }` | Maintenance |
| `cortex.prompt.create` | `{ name, body }` | `{ node_id }` | Prompt management |
| `cortex.prompt.get` | `{ name }` | `{ node }` | Prompt retrieval |
| `cortex.prompt.list` | `{}` | `{ prompts: [...] }` | List prompts |
| `cortex.prompt.set` | `{ name, body }` | `{ node_id }` | Update prompt |
| `cortex.prompt.remove` | `{ name }` | `{ ok: true }` | Soft-delete prompt |
| `cortex.prompt.performance` | `{ name }` | `{ ...stats }` | Observation stats |
| `cortex.agent.list` | `{}` | `{ agents: [...] }` | Agent profiles |
| `cortex.agent.bind` | `{ name, prompt, weight }` | `{ ok: true }` | Bind prompt |
| `cortex.agent.unbind` | `{ name }` | `{ removed }` | Remove bindings |
| `cortex.agent.select` | `{ name }` | `{ prompt, weight }` | Epsilon-greedy select |
| `cortex.observe` | `{ agent, variant_id, sentiment, outcome }` | `{ node_id, new_weight }` | Observation learning |

#### Session Operations

| Method | Params | Response | Notes |
|---|---|---|---|
| `session.search` | `{ query, limit }` | `{ hits: [...] }` | FTS5 search |
| `session.list` | `{ limit }` | `{ sessions: [...] }` | List sessions |
| `session.resume` | `{ session_id }` | `{ ok: true }` | Resume into agent |
| `session.history` | `{ session_id }` | `{ messages: [...] }` | Full message history |

#### Tool Approval

| Method | Params | Response | Notes |
|---|---|---|---|
| `approval.pending` | `{}` | `{ requests: [...] }` | Current approval queue |
| `approval.respond` | `{ request_id, allow }` | `{ ok: true }` | Approve/deny |

#### Diagnostics

| Method | Params | Response | Notes |
|---|---|---|---|
| `daemon.ping` | `{}` | `{ pong, pid, uptime_secs }` | Health check |
| `daemon.shutdown` | `{ force? }` | `{ ok: true }` | Graceful shutdown |

---

## Daemon Lifecycle

### Auto-Start Behavior

CLI and TUI check for `~/.vulcan/vulcan.sock`:

1. Socket exists and `daemon.ping` succeeds → use daemon
2. Socket exists but `connect()` fails → stale socket, remove it
3. No socket → start daemon in background (`vulcan daemon start --detach`), wait for socket, then connect

For users who prefer explicit control: `vulcan daemon start` / `vulcan daemon stop`.
The `--no-daemon` flag on any command forces direct (in-process) mode, same as
current behavior.

### Startup Sequence

```
vulcan daemon start
  ├── Create ~/.vulcan/vulcan.sock (0600)
  ├── Load Config
  ├── Open SessionStore (SQLite)
  ├── Open CortexStore (redb, embedding model, HNSW)
  ├── Build Agent (provider, tools, hooks)
  ├── Start LSP servers (on demand)
  ├── Listen on Unix socket
  └── Write PID to ~/.vulcan/daemon.pid
```

### Shutdown

- **Graceful**: `vulcan daemon stop` or `SIGTERM` → finish in-flight requests, close redb, reap LSP servers, remove socket, exit
- **Crash recovery**: On next start, detect stale socket (connect fails) → remove → start fresh. redb auto-repairs dirty files via `Database::builder().set_repair_callback()`.

### PID File

```
~/.vulcan/daemon.pid
```

Contains the daemon's PID. Used by `vulcan daemon status` and `vulcan daemon stop`.
Stale PID detection: check `/proc/{pid}/comm` or equivalent — if it's not a vulcan
process, the PID file is stale.

---

## Migration Path

### Phase 1: Cortex-Only Daemon (Smallest Scope)

Extract just `CortexStore` + `SessionStore` into a daemon. Solves the immediate
lock problem and the embedding-model cold-start problem. The TUI and `vulcan prompt`
still build their own `Agent` but get `CortexStore` from the daemon.

**Changes:**
- `vulcan daemon` subcommand (start/stop/status)
- Socket listener in daemon process
- `cli_cortex.rs` routes through daemon when socket is alive
- `CortexStore::try_open()` becomes `CortexStore::connect_or_open()` — tries daemon, falls back to direct
- TUI: get `CortexStore` reference from daemon instead of building one in hooks
- Remove transient `RedbStorage` handle hack from `cortex.rs`
- Remove O(N) `stats()` traversal — use daemon's direct storage access

**Doesn't solve:** Agent cold start for `vulcan prompt`, LSP cold start, tool/hook re-init.

### Phase 2: Full Agent Daemon

Move the entire `Agent` into the daemon. TUI and CLI become thin frontends.

**Changes:**
- `prompt.run` / `prompt.stream` methods on daemon
- TUI: replace `Arc<Mutex<Agent>>` with async stream consumer over socket
- CLI `vulcan prompt`: serialize request → socket → print streamed response
- `vulcan search`: `session.search` over socket
- Gateway: route lanes through daemon instead of building per-lane Agents
- Tool approval: daemon sends `approval.request` → TUI shows overlay → TUI sends `approval.respond`
- Model switching: `agent.switch_model` over socket

**Solves:** Everything. One warm Agent, one CortexStore, one LSP pool.

### Phase 3: Multi-Session Daemon

Support multiple concurrent Agent sessions in one daemon (for gateway with
multiple lanes, or TUI with session switching).

**Changes:**
- `session.create` / `session.destroy` methods
- Per-session Agent state in daemon
- Session IDs in request envelope for routing

---

## Tradeoffs

### Pro Daemon

| Benefit | Impact |
|---|---|
| Eliminates cortex lock conflicts | Core usability fix |
| 3-5 second cold start eliminated for `vulcan prompt` | Major UX improvement |
| Embedding model stays loaded (1-2s savings per cortex call) | Noticeable |
| HNSW index stays warm | Faster cortex search |
| LSP servers persist across invocations | Faster code tools |
| CLI works while TUI is running | Core usability fix |
| Gateway lanes share one Agent pool | Resource efficiency |

### Con Daemon

| Cost | Mitigation |
|---|---|
| Socket protocol to design, version, maintain | Start simple (JSON frames), version field from day 1 |
| Daemon lifecycle management (start/stop/crash) | Auto-start + stale socket detection + `--no-daemon` fallback |
| Security: who can connect to socket? | `0600` file perms (same-user only), same as ssh-agent |
| TUI rewrite: `Arc<Mutex<Agent>>` → async stream consumer | Phase 2, incremental — TUI keeps direct mode during migration |
| State mutations become RPCs (model switch, session resume) | Thin wrapper functions, same call sites |
| Risk of daemon state divergence (config hot-reload?) | Daemon watches config file, or restart on `SIGHUP` |
| More code to maintain | But eliminates duplicated init code in 5+ call sites |

### Alternative Considered: Vendor cortex-memory-core + Expose `storage()`

Add `pub fn storage(&self) -> &Arc<RedbStorage>` to `Cortex`. Routes edge
operations through the existing handle instead of transient opens.

- **Fixes:** Same-process edge access (hooks within TUI). Eliminates O(N) `stats()`.
- **Doesn't fix:** Cross-process lock conflict (CLI vs TUI). Still need IPC for that.
- **Verdict:** Worth doing regardless (clean up transient handle hack), but insufficient alone.

---

## Open Questions

1. **Auto-start or explicit?** Should `vulcan prompt` auto-start the daemon, or require the user to run `vulcan daemon start` first? (Recommend: auto-start with `--no-daemon` override.)
2. **Single-session or multi-session daemon?** Phase 2 uses one Agent; Phase 3 adds multi-session. Start with single-session?
3. **Wire format:** JSON over length-delimited frames, or bincode? JSON is debuggable; bincode is faster. (Recommend: JSON first, bincode as optimization later.)
4. **Config hot-reload:** When the user edits `config.toml`, does the daemon pick it up? Or require restart? (Recommend: `SIGHUP` reload + `daemon.reload` method.)
5. **Gateway interaction:** Does the gateway run its own daemon, or connect to the same one? (Recommend: same daemon, multi-session in Phase 3.)
6. **TUI migration strategy:** Rewrite TUI in one shot, or dual-mode (direct + daemon) during transition? (Recommend: dual-mode with feature flag.)
