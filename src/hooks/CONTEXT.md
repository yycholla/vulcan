# Hooks — Context

Five-event hook system: `BeforePrompt`, `BeforeToolCall`, `AfterToolCall`, `BeforeAgentEnd`, `session_start`/`session_end`. Outcomes: `Continue` / `Block` / `ReplaceArgs` / `ReplaceResult` / `InjectMessages { position }` / `ForceContinue`. First non-`Continue` wins for blocking events; injections accumulate.

In-tree precursor to OpenClaw-style plugin architecture.

## Glossary

_Stub — populate via `/grill-with-docs` when area-specific terms accumulate._

## Invariants

1. Long-lived Agent — handlers carry state.
2. `BeforePrompt` injections are transient — `messages` array unchanged.
3. Built-in hooks (`SkillsHook`) registered before `Arc`-wrap; caller hooks (audit) registered before handoff.
