# Extension Details Replay and Frontend Rendering

Extension tools may return structured `ToolResult.details` alongside the LLM-facing `output`. `details` is local structured state: hooks and frontends can read it, stream frames carry it, and saved tool messages preserve it as JSON for session replay. Frontend renderers consume `details` at the `ToolCallEnd` stream boundary and project it into the frontend's native representation. The TUI currently stores the rendered tool-card preview lines in `ChatMessage::output_preview` rather than adding raw `details` to the chat message model.

## Considered Options

- Store raw `details` on every TUI `ChatMessage` and let renderers run during every draw.
- Render once at stream-event handling time and store the resulting preview lines.
- Put extension state only in an out-of-band extension state store and keep tool messages text-only.
- Ask daemon-side extensions to produce generic UI primitives for every frontend.

## Decision

Use `ToolResult.details` as the primary branchable replay contract, because it already travels with tool results and survives session history/fork flows. Frontends render `details` locally and persist only their rendered preview in the current TUI message model. Richer frontend event channels can add live widgets later, but they do not replace the tool-history replay path.

## Consequences

- Session-local extension state can be rebuilt by scanning saved `Message::Tool` entries and reading the latest schema-specific `details` snapshot.
- The TUI avoids a high-blast-radius `ChatMessage` schema expansion while still supporting custom renderers for extension tool results.
- Frontend renderers must tolerate absent or unknown `details` and fall back to the normal tool output path.
- A frontend that wants to re-render historical cards after a renderer upgrade will need either raw event replay or a future message-model migration; the current contract optimizes for stable session replay and low-risk UI integration.
- Daemon code never owns frontend layout. It emits structured tool state; frontend code decides how that state appears locally.
