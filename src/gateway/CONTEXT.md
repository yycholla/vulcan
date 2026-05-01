# Gateway — Context

Bridge to external chat platforms. Owns Discord/Telegram/loopback connectors, inbound + outbound queues (durable SQLite), scheduler, lane routing, per-lane long-lived agents with idle eviction, render registry.

## Glossary

_Stub — populate via `/grill-with-docs` when area-specific terms accumulate._

## See also

- ADR-0001 (daemon-required frontends).
- `src/daemon/CONTEXT.md` — gateway shares daemon client transport.
