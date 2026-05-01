# Context Map

Pointer index for per-area `CONTEXT.md` files. Multi-context layout: skills should read the root `CONTEXT.md` for global glossary, then descend into the relevant area file(s).

## Areas

| Area       | Path                           | Scope                                               |
| ---------- | ------------------------------ | --------------------------------------------------- |
| agent      | `src/agent/CONTEXT.md`         | Long-lived agent core, prompt loop, message state   |
| daemon     | `src/daemon/CONTEXT.md`        | Long-lived background process, IPC, session lifecycle, child sessions |
| extensions | `src/extensions/CONTEXT.md`    | Pi-style extension system — factories, session instances, frontend extensions, manifests, capability gating, state branching |
| gateway    | `src/gateway/CONTEXT.md`       | External platform connectors (Discord, Telegram, loopback), inbound/outbound queues, scheduler |
| hooks      | `src/hooks/CONTEXT.md`         | Five-event hook system, registry, built-in vs caller hooks |
| provider   | `src/provider/CONTEXT.md`      | OpenAI-compatible LLM provider, buffered + streaming paths |
| tools      | `src/tools/CONTEXT.md`         | Tool trait, dispatch, `BeforeToolCall`/`AfterToolCall` integration |
| tui        | `src/tui/CONTEXT.md`           | Terminal UI, session ownership, `Arc<Mutex<Agent>>` lifecycle |

Other `src/` directories (memory, knowledge, skills, playbook, policy, trust, orchestration, platform, replay, review, snapshots, etc.) fold under the nearest listed area or are out-of-scope for the map until they accumulate domain language worth glossarising.

## Cross-cutting

- **Root `CONTEXT.md`** — global glossary, project-wide concepts.
- **`docs/adr/`** — system-wide ADRs.
- **`src/<area>/docs/adr/`** — area-scoped ADRs (create lazily when needed).
