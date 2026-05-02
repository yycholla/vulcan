# TUI — Context

Terminal UI. Holds the Agent in `Arc<tokio::sync::Mutex<Agent>>` for the whole session — never construct a fresh Agent per prompt.

TUI mode logs to file (so `tracing` doesn't splat the screen); one-shot mode logs to stderr.

## Glossary

**UiRuntime**:
The retained TUI runtime that owns transient foreground surfaces such as extension canvases, future drawers, popups, and modals. Ratatui still renders immediate-mode frames; `UiRuntime` decides which surfaces exist, which surface has focus, which prior surface regains focus, and which surfaces receive input/ticks.
_Avoid_: view branch, overlay booleans, widget registry

**Surface**:
A transient TUI interaction layer owned by `UiRuntime`. A surface can render on top of or beside a normal view and can receive focused input independently from chat prompt entry.
_Avoid_: page, screen, canvas unless specifically referring to extension canvases

**Surface Placement**:
The resolved rectangle policy for a Surface, such as fullscreen, centered modal, or edge drawer. Renderers ask for placement instead of hand-calculating coordinates.
_Avoid_: inline rect math, overlay dimensions

**View**:
A built-in persistent TUI layout such as single stack, split sessions, tiled mesh, tree of thought, or trading floor. Views are not transient surfaces and remain selected behind any active Surface.
_Avoid_: surface, frontend extension

**Layout**:
A file-level page composition module that arranges Widgets and Surfaces for one built-in View, such as `layouts/single_stack.rs` or `layouts/trading_floor.rs`.
_Avoid_: dumping all page composition into `views.rs`

**Widget**:
A reusable Ratatui element implemented as a concrete `Widget` struct when it renders into a `Rect`, such as prompt row, ticker, provider picker, tool card, or panel chrome.
_Avoid_: ad hoc render helper when the element has stable inputs and visual identity

**Prompt Editor**:
The multi-line, Vim-mode text editor at the bottom of the TUI. It owns text-editing state and mirrors submitted text into the legacy prompt string until the TUI event loop is fully disentangled.
_Avoid_: input buffer, prompt row
