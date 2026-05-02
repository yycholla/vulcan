# ADR-0007: Symphony Workflow Contract

## Status

Accepted

## Context

Symphony needs a repo-owned contract for unattended coding-agent work. The service must load runtime policy and the task prompt from the repository rather than from hidden operator state, but the first implementation slice must not pull in typed runtime config, task-source adapters, workspace creation, or agent execution.

## Decision

Use `WORKFLOW.md` as the workflow contract. Optional YAML front matter is parsed as the root configuration map and the trimmed Markdown body is the prompt template. Unknown front matter keys are preserved for later config and source-specific slices.

Prompt rendering is strict. Unknown variables, unknown filters, malformed tags, and non-iterable loops fail instead of producing ambiguous prompts. The render context exposes a tracker-independent normalized task under `issue` plus optional `attempt` metadata for retries and continuations.

## Consequences

- Workflow read and YAML/front-matter errors are configuration errors that block dispatch.
- Template parse/render errors are run-attempt errors and can be retried or surfaced by later orchestrator slices.
- Later config slices can layer typed validation over the preserved front matter without changing this loader contract.
- Later task-source slices must normalize tracker payloads before prompt rendering.
