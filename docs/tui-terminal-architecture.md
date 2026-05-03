# TUI Terminal Architecture

The TUI terminal layer should be a deep module: callers get a Ratatui frame target,
input events, and a capability snapshot without knowing which terminal backend is in
use.

## Current Slice

- Rendering uses Ratatui's Termwiz backend.
- `src/tui/init.rs` owns backend construction, raw mode, alternate screen, and the
  capability snapshot.
- Termwiz capability detection is normalized into `TerminalCapabilities` so UI code
  can ask Vulcan-level questions instead of depending on Termwiz types.
- Mouse capture remains disabled by default. Termwiz enables mouse reporting from
  capabilities during raw mode, so the capability hints explicitly force mouse
  reporting off until Vulcan has an opt-in mouse mode.

## Seams

### Terminal Session

`TerminalSession` is the lifecycle seam. It owns:

- `TuiTerminal`: the concrete Ratatui terminal backend.
- `TerminalCapabilities`: normalized terminal features such as color level,
  bracketed paste, hyperlinks, Sixel, iTerm2 images, and background color erase.

The session exists so future rendering features can depend on capability policy
without spreading backend-specific queries through views and widgets.

### Input Adapter

The next deepening step is a local input adapter:

```text
Termwiz/crossterm raw input
  -> TuiInputEvent
  -> existing prompt, picker, canvas, and keybind handlers
```

Today the TUI still accepts crossterm key events directly in several modules. That
keeps this backend switch small, but it is intentionally temporary. The adapter
should preserve existing keybind tests while allowing Termwiz-specific events such as
pixel mouse, richer key encodings, wake events, and resize events.

### Capability Policy

Advanced features should depend on `TerminalCapabilities`, not environment checks in
widgets. Examples:

- render hyperlinks only when `hyperlinks` is true;
- allow inline image protocols only when `sixel` or `iterm2_image` is true;
- choose color depth from `color_level`;
- keep mouse features opt-in until normal terminal selection is preserved.

## Migration Order

1. Switch rendering backend and capture normalized capabilities.
2. Add `TuiInputEvent` plus crossterm-to-local conversion, leaving behavior
   unchanged.
3. Replace crossterm input polling with Termwiz polling inside the adapter.
4. Route resize, wake, pixel mouse, and future advanced input through the same event
   type.
5. Gate visual affordances and media experiments through `TerminalCapabilities`.

## Extension Surface ABI

Extension commands and async extension events use the same dispatch envelope:

```text
FrontendCommandDispatch
  action
  widget_updates
  tick_requests
  canvas_requests
  surface_requests
  surface_updates
  surface_closes
```

`OpenSurface` is the canonical command action for extension UI. `OpenView` remains
as a compatibility adapter and is converted into a modal `FrontendSurface` by the
TUI host.

Supported surface placements:

- `Modal`: centered, compact, opaque.
- `Fullscreen`: occupies the view area.
- `RightDrawer`: anchored to the right edge.
- `BottomDrawer`: anchored to the bottom edge.

Surface lifecycle:

- `OpenSurface(FrontendSurface)` mounts or replaces a surface with the same id.
- `UpdateSurface(FrontendSurfaceUpdate)` updates title, body, or placement for an
  existing text surface.
- `CloseSurface { id }` closes the matching mounted surface.
- `ExtensionUi::open_surface`, `update_surface`, and `close_surface` provide the
  same operations for async extension events.

Current limitation: `FrontendSurface` is a text surface. Rich interactive
extension canvases still use `ExtensionUi::custom(CanvasFactory)` and receive
canvas key/tick callbacks. The intended next ABI step is typed interactive
surfaces that receive focused key events without exposing Ratatui internals.

Close policy lives on the mounted surface spec. Text surfaces are modal blockers
and close on `Esc` or `Ctrl+C`; approval surfaces are modal blockers and deny on
cancel; canvases close on `Esc`/`Ctrl+C` without blocking turn cancellation.
