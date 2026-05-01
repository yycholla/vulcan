# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Layout: multi-context

This repo uses a **multi-context** layout:

- `CONTEXT-MAP.md` at the root — index pointing to per-area `CONTEXT.md` files.
- `CONTEXT.md` at the root — global glossary and cross-cutting domain language.
- `src/<area>/CONTEXT.md` — per-area domain notes (agent, daemon, gateway, hooks, provider, tools, tui).
- `docs/adr/` — system-wide architectural decisions.
- `src/<area>/docs/adr/` — area-scoped decisions (created lazily when needed).

## Before exploring, read these

- **`CONTEXT-MAP.md`** at the repo root — pick the area(s) relevant to the topic.
- **Root `CONTEXT.md`** — global glossary.
- **Per-area `CONTEXT.md`** — for each area you'll touch.
- **`docs/adr/`** — read ADRs that touch the area you're about to work in.
- **`src/<area>/docs/adr/`** — area-scoped decisions if present.

If any of these files don't exist, **proceed silently**. Don't flag their absence; don't suggest creating them upfront. The producer skill (`/grill-with-docs`) creates them lazily when terms or decisions actually get resolved.

## File structure

```
/
├── CONTEXT-MAP.md
├── CONTEXT.md
├── docs/adr/                          ← system-wide decisions
│   ├── 0001-daemon-required-frontends.md
│   ├── 0002-shared-runtime-resource-pool.md
│   ├── 0003-extension-daemon-frontend-split.md
│   ├── 0004-extension-distribution-and-lifecycle.md
│   └── 0005-extension-compaction-control.md
└── src/
    ├── agent/
    │   └── CONTEXT.md
    ├── daemon/
    │   └── CONTEXT.md
    ├── gateway/
    │   └── CONTEXT.md
    ├── hooks/
    │   └── CONTEXT.md
    ├── provider/
    │   └── CONTEXT.md
    ├── tools/
    │   └── CONTEXT.md
    └── tui/
        └── CONTEXT.md
```

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a hypothesis, a test name), use the term as defined in `CONTEXT.md` (root or per-area). Don't drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal — either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `/grill-with-docs`).

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently overriding:

> _Contradicts ADR-0003 (extension daemon/frontend split) — but worth reopening because…_
