# TUI — Context

Terminal UI. Holds the Agent in `Arc<tokio::sync::Mutex<Agent>>` for the whole session — never construct a fresh Agent per prompt.

TUI mode logs to file (so `tracing` doesn't splat the screen); one-shot mode logs to stderr.

## Glossary

**Surface Stack**:
The retained TUI module that owns transient foreground surfaces such as extension canvases, future drawers, popups, and modals. Ratatui still renders immediate-mode frames; the Surface Stack only decides which surfaces exist, receive input/ticks, and close.
_Avoid_: view branch, overlay booleans, widget registry

**Surface**:
A transient TUI interaction layer owned by the Surface Stack. A surface can render on top of or beside a normal view and can receive focused input independently from chat prompt entry.
_Avoid_: page, screen, canvas unless specifically referring to extension canvases

**View**:
A built-in persistent TUI layout such as single stack, split sessions, tiled mesh, tree of thought, or trading floor. Views are not transient surfaces and remain selected behind any active Surface.
_Avoid_: surface, frontend extension

**Prompt Editor**:
The multi-line, Vim-mode text editor at the bottom of the TUI. It owns text-editing state and mirrors submitted text into the legacy prompt string until the TUI event loop is fully disentangled.
_Avoid_: input buffer, prompt row
