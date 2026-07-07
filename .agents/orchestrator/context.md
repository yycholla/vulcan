# Context: Daemon-only Frontends for CLI and TUI

## Goals & Constraints

- Implement daemon-only frontend changes for CLI and TUI.
- Fix compiler errors caused by replacing `Arc<Mutex<Agent>>` with `Arc<TuiBackend>` in `src/tui/*.rs`.
- Ensure unsupported daemon client features handle `None` gracefully.
- Write two regression tests: (1) connection failure shows log path; (2) --no-daemon works as escape hatch.

## Target Areas

- `src/tui/backend.rs`
- `src/tui/mod.rs`
- `src/cli/` (daemon connection/configuration paths)
- Regression tests locations (e.g. `tests/` or unit tests).

## Known Issues (Compilation Errors)

- `src/tui/backend.rs`: `StreamEvent::ToolCallEnd` missing `elided_lines` and `output_preview` fields in struct initializer.
- `src/tui/mod.rs`:
  - Mismatched types for `app.diff_sink` (expected `Arc<DiffSink>`, got `Option<Arc<DiffSink>>`).
  - `active_profile()` returns a future yielding an option, need to await it before calling `.map()`.
  - Type annotations needed for `profile_label`.
  - `a.memory()` method not found on `&TuiBackend`.
  - `a.available_models()` method not found on `&TuiBackend`.
