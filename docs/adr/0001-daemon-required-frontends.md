# Daemon-Required Frontends

Vulcan frontends must connect to the long-lived daemon instead of constructing an in-process agent fallback. This is harder on daemon auto-start and socket reliability, but it gives one process ownership of the runtime resource pool, avoids split-brain behavior between direct and daemon paths, and eliminates repeated cold-start work and redb lock conflicts.

## Considered Options

- Daemon-required frontends.
- Daemon-preferred frontends with direct in-process fallback.

## Consequences

- CLI, TUI, gateway, and future connectors share daemon client behavior.
- Daemon client behavior includes request-id multiplexing: one socket can carry concurrent normal calls, streaming turn frames, and `id: null` daemon push frames.
- Subagents should run as child sessions rather than direct child agents.
- Failures to auto-start or connect to the daemon are user-visible errors, not a reason to silently rebuild direct-mode runtime state.
