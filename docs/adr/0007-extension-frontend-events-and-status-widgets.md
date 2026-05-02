# Extension Frontend Events and Status Widgets

Daemon-side extensions need a live frontend channel for progress that is not part of durable session history. Tool results remain the replay contract; live widgets are transient frontend state.

## Considered Options

- Persist widget state in session history.
- Add daemon-owned widget primitives to every extension event.
- Route daemon extension events through an extension-owned JSON payload and let frontend extensions map them locally.
- Treat all frontend events as request-correlated stream frames.

## Decision

Use an out-of-band daemon push frame for frontend events:

- Envelope: `{ "kind": "extension_event", "session_id": "...", "extension_id": "...", "payload": ... }`.
- `payload` is extension-owned JSON. The daemon does not interpret widget shape.
- The frontend demuxes by `extension_id` and invokes `FrontendCodeExtension::on_event(payload, ctx)`.
- Frontend extensions set transient status widgets with `ctx.ui.set_widget(id, Option<WidgetContent>)`.

Activation is frontend-aware. If a daemon extension declares required frontend capabilities, the session activates it only when the connected frontend provides those capabilities. If a frontend extension with the same id reports a different version, activation marks the daemon extension `Broken` with `extension version mismatch`.

## Consequences

- Live UI does not pollute saved chat history or `ToolResult.details` replay.
- Daemon extensions can emit progress without owning layout.
- Frontend extensions own presentation and can ignore unknown payloads safely.
- Request-correlated text/tool frames and out-of-band push frames share the same protocol but keep different demux paths.
- Non-frontend CLI sessions still work because extensions without frontend requirements activate normally; frontend-required extensions simply do not activate without the required capability.
