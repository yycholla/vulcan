# Shared Runtime Resource Pool

The daemon owns one runtime resource pool for expensive and global adapters, while each session owns only conversation-specific state. This avoids rebuilding provider catalog/cache infrastructure, cortex memory, LSP processes, stores, and heavy tool adapters for every session while still keeping session history, provider selection, tool registry filtering, hook instances, cancellation, and active turns isolated per session.

## Considered Options

- One shared runtime resource pool with per-session state.
- Fully isolated full-stack sessions where each session builds its own stores, tools, hooks, cortex, and LSP resources.

## Consequences

- Session construction must assemble session-local interfaces from daemon-owned adapters instead of calling an all-in-one agent builder.
- Hook instances and tool registries remain session-local, but their factories and heavy dependencies come from the daemon.
- Store interfaces stay separate by domain, but share daemon-owned storage resources where appropriate.
- Frontends should reuse a multiplexed daemon client so many interactions can share the daemon-owned resources without opening one socket per call or blocking calls behind a stream.
