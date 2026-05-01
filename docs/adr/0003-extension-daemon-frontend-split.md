# Extension Daemon/Frontend Split

Vulcan extensions split into two registration sites. A daemon-side **Daemon Extension Factory** is registered once at daemon startup and instantiated per **Session** as a **Session Extension** that owns hooks, tools, slash commands routed to the LLM, providers, and lifecycle handlers. A frontend-side **Frontend Extension** is registered once at frontend binary startup and owns rendering, custom canvases, raw input capture, status widgets, autocomplete, and slash commands routed to local UI. Cross-side communication flows over the existing daemon push frame protocol: tool result `details` ride the **Turn Event** stream, and a bespoke `ctx.emit_frontend_event(payload)` channel covers state changes that aren't tied to a tool call. Either side may be absent in a given crate so pure-daemon (e.g. auto-commit) and pure-frontend (e.g. DOOM) extensions are first-class.

## Considered Options

- Daemon/frontend split as described.
- Daemon-only extensions with a daemon-side render protocol that ships serializable scene primitives to whichever frontend is connected.
- Frontend-only extensions that drive the daemon via existing socket RPCs.
- Free-form `surface` declaration per command/tool/renderer with extensions running on whichever side they please.

## Consequences

- The daemon never owns rendering state or input event interpretation. **Frontend Capability** declarations gate extension activation per **Session** so DOOM-class extensions stay inert on chat-platform lanes without special-casing.
- A single extension crate that wants both surfaces uses Cargo features (`daemon`, `tui`) to gate its respective `inventory::submit!` site. Shared types live in the always-compiled module so the daemon-side and frontend-side modules statically reference the same wire format.
- Extension version skew between daemon module and frontend module of the same crate is detected at handshake and surfaced through the existing `ExtensionStatus::Broken` path with `mark_broken`.
- Built-in tools live in an unprefixed namespace; extension tools must be prefixed (default `<id>_`); name collisions across active extensions short-circuit the second registration to `Broken`.
- Future Phase 4 dynamic-loading targets (subprocess, WASM, native) fold into the same `DaemonCodeExtension` factory pattern so the in-tree wire format and the dynamic-load wire format share serde shapes.
