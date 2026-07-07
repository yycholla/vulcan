# Project Plan: Daemon-only frontends for CLI and TUI

## Architecture

- CLI and TUI modules in `src/cli/` and `src/tui/`.
- Backend communication routes through `TuiBackend` enum which wraps either a direct Agent or a daemon client connection.

## Milestones

| #   | Name                                  | Scope                                                                                                                 | Dependencies | Status      |
| --- | ------------------------------------- | --------------------------------------------------------------------------------------------------------------------- | ------------ | ----------- |
| 1   | Exploration & Analysis                | Identify all compilation issues and unsupported daemon client methods/features.                                       | None         | DONE        |
| 2   | Resolve TuiBackend Compilation Errors | Fix all compilation errors in TUI modules, handling None/stub values gracefully in the daemon client path.            | M1           | IN_PROGRESS |
| 3   | Add PRD Regression Tests              | Add tests ensuring: (1) connection failure shows log path and errors out; (2) --no-daemon bypasses daemon connection. | M2           | PLANNED     |
| 4   | Final Verification & Validation       | Run check, tests, and TUI run validation. Run Challenger and Auditor checks.                                          | M3           | PLANNED     |

## Interface Contracts

- `TuiBackend`: enum/struct providing uniform interface for TUI interactions (e.g. active models, session id, active profile, memory, etc.).
- When backend is daemon client, unsupported features (like memory/search, diff_sink, pricing) should return `None` or be stubbed gracefully.
