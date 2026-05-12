---
title: Extensions — Feature Specification
type: feature
status: proposed
phase: Phase 3 planning spec
created: 2026-04-29
updated: 2026-05-08
tracking: GitHub #265; Linear YYC-165 / YYC-212 historical refs
tags: [extensions, promotion, lifecycle]
---

# Extensions

## Status

| Field | Value |
|---|---|
| Status | Proposed Phase 3 spec |
| Current implementation state | foundation only: skills and hooks are shipped; registry/metadata/draft-extension parsing/code-backed extension skeletons exist; dynamic loading/store marketplace remain proposed |
| Tracking | GitHub #265; Linear YYC-165 / YYC-212 historical refs |
| Dependencies / non-goals | Hook system, SkillsHook, and local extension registry foundations. This document does not claim the proposed behavior is currently available. |

> Language note: sections below describe the target design. Unless the status table explicitly calls out a shipped foundation, read capability statements as proposed behavior.


Proposed extensions are the next tier above the shipped **skills** foundation. While skills are flat markdown files that inject prompt text, extensions are structured components with richer metadata, configurable parameters, and — eventually — programmatic hook handlers compiled into or loaded by the agent.

In the proposed promotion ladder, an extension starts as a skill. After proving its utility across sessions, it can be promoted.

## Skill → Extension promotion

See [`docs/features/skills.sh.md`](./skills.sh.md) for full promotion criteria and path. In summary:

| | Skill | Extension |
|---|---|---|
| **Format** | Flat markdown + YAML frontmatter | Structured metadata + optional code handler |
| **Storage** | `~/.vulcan/skills/` | `~/.vulcan/extensions/` |
| **Lifecycle** | Loaded at boot by `SkillRegistry` | Registered via `ExtensionRegistry` |
| **Behavior** | Prompt injection only (`SkillsHook`) | Prompt injection + hook handler + config |
| **Persistence** | None (read from disk each boot) | May carry persistent state across sessions |

### Draft extension stage

Before writing Rust code, a skill could be promoted to a **draft extension** by adding extended frontmatter:

```markdown
---
name: deploy-check
description: Pre-deployment validation checklist
triggers: ["deploy", "release", "ship"]
extension: candidate
extension_confidence: 0.7
config_schema:
  type: object
  properties:
    required_checks:
      type: array
      items:
        type: string
    notify_channel:
      type: string
  required: ["required_checks"]
depends: ["bash", "git"]
---
```

This extended metadata allows the agent to:
- Validate configuration at load time
- Surface richer information in the TUI
- Gate activation on available tools or data sources
- Collect usage metrics for promotion decisions

## Extension registry

In the proposed local lifecycle, extensions would live in `~/.vulcan/extensions/`. The `ExtensionRegistry` would mirror `SkillRegistry` but add:

- **Config validation** — Each extension's `config_schema` is validated against user-provided settings in `~/.vulcan/config.toml`.
- **Dependency checking** — The registry would verify that all `depends` tools are available before activating an extension.
- **Priority ordering** — Extensions could declare a priority to control injection order (higher priority = closer to the system prompt).

## Future: code-backed extensions

The long-term vision (Phase 3 per the master plan) is for extensions to have compiled Rust hook handlers — either in-tree (built into the binary) or dynamically loaded. This mirrors the OpenClaw plugin architecture referenced in [`~/wiki/queries/rust-hermes-plan.md`](../../wiki/queries/rust-hermes-plan.md).

```rust
// Future Extension trait sketch
pub trait Extension: Send + Sync {
    fn name(&self) -> &str;
    fn config_schema(&self) -> Option<Value>;
    fn hook_handler(&self) -> Option<Box<dyn HookHandler>>;
}
```

Until dynamic loading lands, code-backed extensions live in `src/extensions/` and are registered at compile time.

## Relationship to hooks

Extensions build on the **hook system** (`src/hooks/`). A promoted extension that needs programmatic behavior registers a `HookHandler` on one or more of the five events:

- `BeforePrompt` — inject context, instructions, or data
- `BeforeToolCall` — validate or block tool usage
- `AfterToolCall` — observe or rewrite tool results
- `BeforeAgentEnd` — force a continuation
- `session_start` / `session_end` — lifecycle observability

Skills use the hook system indirectly via `SkillsHook`. Extensions use it directly.

## Future: dynamic extension store

The long-term vision for packaged, installable extensions with cryptographic signing, sandboxed runtimes (WASM, native dynamic libraries, scripting), and a remote repository index is documented separately in [`docs/features/extension-store.md`](./extension-store.md).

The promotion path described here (skill→draft extension→code extension) feeds into that system: a promoted code extension is the same `Extension` trait that the store's dynamic loaders ultimately instantiate.
