# Original User Request

## Initial Request — 2026-07-07T22:23:29Z

Finish the implementation of issue #706 ("Daemon-only frontends for CLI and TUI") in the `vulcan` repository. Specifically, fix the compiler errors caused by replacing the direct `Agent` usage with the new `TuiBackend` enum in the TUI, ensuring the TUI successfully compiles and routes operations through the backend.

Working directory: /home/yycholla/Documents/vulcan
Integrity mode: benchmark

## Requirements

### R1. Resolve `TuiBackend` Compiler Errors

Fix all remaining type and method-resolution errors in the TUI module (`src/tui/*.rs`) caused by replacing the `Arc<Mutex<Agent>>` with `Arc<TuiBackend>`. Ensure that any unsupported features in the daemon client path (e.g. `diff_sink`, `pricing`) gracefully handle `None` values without breaking the TUI event loop.

### R2. Add PRD Regression Tests

Implement the two specific CLI regression tests required by PRD #706:

1. Ensure daemon connection failure produces a clear error containing the daemon log path (or similar), with no silent fallback to direct mode.
2. Ensure explicit `--no-daemon` still works as the development escape hatch.

## Acceptance Criteria

### Programmatic Verification

- [ ] `rtk cargo check --all-targets` passes with no errors.
- [ ] `rtk cargo test` passes, including the new PRD regression tests.
- [ ] Running the TUI with `rtk cargo run -- chat` does not immediately panic on startup.
