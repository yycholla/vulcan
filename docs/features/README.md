---
title: Feature Specs Roadmap
status: proposed
phase: Phase 3 planning index
created: 2026-05-08
updated: 2026-05-08
tracking: GitHub #264; Linear YYC-25 / YYC-29 / YYC-68 historical roadmap refs
tags: [features, roadmap, phase-3]
---

# Feature Specs Roadmap

## Status

| Field | Value |
|---|---|
| Status | Proposed Phase 3 planning index |
| Current implementation state | foundation only: Vulcan has shipped hooks, skills, audit/safety hooks, SQLite session persistence, extension registry/store/state foundations, MCP adapter modules, TUI/daemon foundations, and observability slices; the cross-feature behaviors below remain proposed unless a linked spec says otherwise. |
| Tracking | GitHub #264; Linear YYC-25 / YYC-29 / YYC-68 historical roadmap refs |
| Dependencies / non-goals | This index sequences planning specs. It does not implement product behavior or replace GitHub Issues as the tracking source of truth. |

## Sequencing

1. Shipped foundations to preserve: hooks, safety/approval/audit, skills, session persistence, runtime resource pool, TUI/daemon surfaces, extension registry/store/state primitives, and MCP tool-adapter primitives.
2. Extension platform foundation: [extensions](./extensions.md), [skills](./skills.sh.md), [governance and safety](./governance-safety.md), and [extension store](./extension-store.md) should stay conservative and local-first.
3. Developer workflow: [developer experience](./developer-experience.md) depends on the local manifest/store and should not imply remote publishing before verification and install-state semantics are stable.
4. Interoperability: [MCP server support](./mcp-server-support.md) and [MCP as an extension](./mcp-as-extension.md) should start with governed stdio/client/tool adaptation before managed hosting, sampling, or store-driven MCP installs.
5. Stateful and agentic behavior: [persistence/stateful extensions](./persistence-stateful-extensions.md) should land before [agentic extensions](./agentic-extensions.md), because autonomous extension behavior needs scoped state, audit, policy, and budget controls.
6. UX and ecosystem: [UX extensions](./ux-extensions.md), [ecosystem integrations](./ecosystem-integrations.md), and [marketplace discovery](./marketplace-discovery.md) are later slices. Remote marketplace/discovery depends on local package semantics, trust policy, and sandbox runtime decisions.

## Open questions from `~/wiki/queries/rust-hermes-plan.md`

- The master plan lists Phase 3 "Plugins (dynamic loading via dlopen or WASM)". The issue audit and these specs prefer a staged trust ladder: in-process first-party foundations, WASM/subprocess/MCP for untrusted code, and native dynamic loading only for trusted first-party/internal use. Open question: should native dylib loading remain a first-class goal or be demoted to a trusted-only escape hatch?
- The master plan says "MCP client/server" as Phase 3. The current codebase now has MCP foundation modules, while the proposed docs still describe managed hosting, resource templates, and sampling as future. Open question: where should the docs draw the line between shipped MCP foundations and unshipped managed MCP behavior?
- The master plan frames browser automation, live canvas/A2UI, and rich tool ecosystems as Phase 3 peers. The current feature set is extension-heavy. Open question: should browser/live-canvas work become separate feature specs or remain out of scope for this extension-roadmap pass?
- The master plan still contains historical `ferris` references. These docs use `vulcan`; remaining naming drift should be fixed when source-of-truth planning notes are next revised.

## Source-of-truth posture

These files remain repo planning specs tied to GitHub/Linear tracking. No cross-cutting design doc was promoted or moved to `~/wiki/queries/` in this pass; `~/wiki/queries/rust-hermes-plan.md` remains the higher-level master vision.
