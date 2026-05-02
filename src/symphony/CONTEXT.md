# Symphony — Context

Long-running scheduler/runner service for coding-agent work. Symphony reads eligible tasks from a configured source, prepares an isolated workspace, launches an agent worker from that workspace, and reports observable run state. It is first-party Vulcan code but remains separate from the normal Daemon turn path in v1.

## Glossary

**Workflow File**:
Repository-owned `WORKFLOW.md` contract. Optional YAML front matter carries runtime configuration; the Markdown body is the prompt template rendered for a normalized task.
_Avoid_: hidden service config, agent-only instructions

**Workflow Front Matter**:
The root YAML map at the top of a **Workflow File**. Unknown top-level keys are preserved so future config slices and source-specific extensions can define their own schema.
_Avoid_: nested config blob

**Workflow Prompt Template**:
The trimmed Markdown body of a **Workflow File**. It renders with strict variable/filter behavior against normalized task data and attempt metadata.
_Avoid_: best-effort prompt string

**Normalized Task**:
Tracker-independent task payload exposed to workflow templates. It carries stable fields such as identifier, title, body, state, labels, blockers, URL, and source-specific references.
_Avoid_: GitHub issue model, Linear issue model

**Attempt Metadata**:
Per-run retry or continuation context passed to the **Workflow Prompt Template**. First attempts use no attempt value; retry and continuation runs may pass an integer.
_Avoid_: hidden retry counter

**Effective Config**:
Typed Symphony runtime view derived from **Workflow Front Matter**. It centralizes defaults, environment indirection, path normalization, startup validation, and poll-time dispatch validation.
_Avoid_: ad hoc front-matter reads in orchestrator modules

**Last Known Good Config**:
The most recent **Effective Config** that passed validation. Invalid poll-time reloads keep using this value and retain the validation error for operator-visible diagnostics.
_Avoid_: crash-on-reload, partial config mutation

## Relationships

- Symphony is daemon-adjacent orchestration, not part of the user-facing **Daemon** session/turn path.
- The **Workflow File** is loaded before dispatch; file read and YAML errors block new work.
- The **Effective Config** is built from preserved front matter after workflow load; startup validation is fatal, while poll-time validation can skip new dispatch and keep the **Last Known Good Config**.
- Template render failures are run-attempt failures, not workflow-file load failures.
- Task-source adapters normalize source payloads before rendering. The workflow layer does not know GitHub, Linear, markdown tasks, or agent todos.
- Workspace management and agent process execution are later slices; this area currently owns only workflow loading and prompt rendering.

## ADRs

- `docs/adr/0007-symphony-workflow-contract.md` — repository-owned workflow contract and strict prompt boundary.
- `docs/adr/0008-symphony-typed-config.md` — typed config defaults, validation, and last-known-good reload semantics.
