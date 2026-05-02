# Extensions — Context

Pi-mono–style extension system. Daemon-side and frontend-side extensions distribute as Cargo crates, register via `inventory`, instantiate per **Session** (daemon side) or per **Frontend** process (frontend side). The current registry (`src/extensions/registry.rs`) is the metadata and code-extension foundation; the daemon/frontend split, wide event bus, and lifecycle policy land via the PRD at GitHub issue #548.

## Glossary

**Daemon Extension Factory**:
A daemon-global registration that knows how to instantiate per-session daemon-side extension state — hooks, tools, LLM-routed commands, providers, lifecycle observers. Lives in the **Runtime Resource Pool**; one entry per installed extension crate that ships a daemon module, registered once at daemon startup.
_Avoid_: shared extension instance, global hook handler

**Session Extension**:
A per-**Session** instantiation of a **Daemon Extension Factory**, owning that session's hook instances, tool entries, lifecycle handlers, and daemon-side extension-local state. Render concerns belong to the **Frontend Extension**, not the **Session Extension**.
_Avoid_: daemon-side renderer, in-session UI handler

**Frontend Extension**:
A per-**Frontend**-process registration that owns rendering, custom canvases, raw input capture, and frontend-routed slash commands. Registered at frontend binary startup; consumes daemon push frames addressed to its extension id but never owns agent state.
_Avoid_: TUI hook, daemon renderer

**Extension Manifest**:
A package-level metadata block (`[package.metadata.vulcan]`) declaring an extension's id, version, capabilities, and optional `daemon_entry` / `frontend_entry` registration symbols. Either entry may be absent for pure-daemon (e.g. auto-commit) or pure-frontend (e.g. DOOM) extensions.
_Avoid_: extension config, package.json

**Frontend Capability**:
A tag declared by a **Frontend** (or a gateway lane connector) at connection-open time describing what user-interaction or rendering surface that frontend supports. The **Daemon** activates an extension on a **Session** only when the connection's declared capabilities cover the **Extension Manifest**'s `requires` list.
_Avoid_: feature flag, runtime capability

**Extension State**:
Per-**Session**, per-extension state. Lives in two places: `ToolResult.details` (pi-style, branches with **Session History** when a **Child Session** forks) and the daemon-owned **Extension State Store** (out-of-band rows keyed by session + extension + key, branched explicitly on fork unless the **Extension Manifest** opts out).
_Avoid_: extension memory, extension cache

**Frontend Renderer**:
A **Frontend Extension** handler that maps a known tool result shape into frontend-native lines or widgets. In the TUI today, renderers consume `ToolResult.details` from the streamed `ToolCallEnd` event and project it into the existing tool-card preview; the durable chat message stores the rendered preview, not the raw details.
_Avoid_: daemon renderer, persistent UI schema

**Frontend Event**:
A daemon-to-frontend push frame with `kind = "extension_event"`, an `extension_id`, and an extension-owned `payload`. The daemon only wraps and routes the payload; the matching **Frontend Extension** interprets it through `on_event(payload, ctx)`.
_Avoid_: daemon UI command, frontend hook event

**Status Widget**:
A small frontend-owned footer/status item set by a **Frontend Extension** via `ctx.ui.set_widget(id, content)`. The daemon-side extension emits frontend events; the frontend extension maps those events to `Text`, `Spinner`, or `Progress` widget updates and clears them with `None`.
_Avoid_: daemon status row, persistent widget state

**Frontend Command**:
A slash command handled entirely by the **Frontend** before the prompt reaches the daemon. Frontend commands produce local UI actions such as opening a view; commands that need the LLM or daemon state remain daemon/session commands.
_Avoid_: hidden tool call, daemon-routed UI command

## Relationships

- Every installed extension that ships a daemon module contributes one **Daemon Extension Factory** to the **Runtime Resource Pool**; the factory is registered once at daemon startup.
- Each new **Session** instantiates a **Session Extension** from each active **Daemon Extension Factory**; hook instances, tool entries, and daemon-side state live on the **Session Extension**, not the factory.
- Session extension tools are registered through the runtime bridge with the canonical `<extension_id>_<tool_name>` prefix. Built-in tools remain unprefixed; extension tool collisions mark the extension `Broken`.
- `ToolResult.details` is the replay contract for session-local extension state. A **Session Extension** may rebuild in-memory state by scanning saved `Message::Tool` history for its own latest details snapshot.
- A **Frontend** owns its own **Frontend Extension** registry; daemon and frontend extensions are linked into separate binaries even when they ship from the same crate.
- A **Frontend Extension** consumes daemon push frames addressed to its extension id; it never owns agent state and never receives hook events directly.
- An extension activates on a **Session** only when the connected **Frontend**'s declared capabilities satisfy the **Extension Manifest**'s required surface.
- When a daemon extension and frontend extension share an id, their versions must match for that session; a mismatch marks the extension `Broken` with `extension version mismatch`.
- Frontend event payloads are intentionally extension-owned JSON. The stable daemon contract is the envelope, not a shared widget schema.
- Renderer collisions are resolved in frontend registry order with last-active wins plus a warning; this mirrors extension load order and avoids daemon-side renderer arbitration.

## ADRs

- `docs/adr/0003-extension-daemon-frontend-split.md` — daemon/frontend split rationale.
- `docs/adr/0004-extension-distribution-and-lifecycle.md` — Cargo crate distribution + mid-session enable/disable/kill policy.
- `docs/adr/0005-extension-compaction-control.md` — extension control over compaction with validation safety net.
- `docs/adr/0006-extension-details-replay-and-frontend-rendering.md` — `ToolResult.details` as replay state plus frontend projection boundary.
- `docs/adr/0007-extension-frontend-events-and-status-widgets.md` — daemon push events routed to frontend extensions plus local status widgets.
