---
task_source:
  kind: markdown
  markdown:
    path: ../tasks/test-workflow.md
polling:
  active_states: ["ready-for-agent"]
  max_concurrent: 1
workspace:
  root: ../workspaces
codex:
  command: codex
  args: ["--profile", "symphony"]
---
You are designing a Symphony workflow for {{ issue.identifier }}: {{ issue.title }}.

Goal: guide the user from rough intent to a complete, reviewable Symphony workflow specification before writing workflow files.

Run this process:

1. Specification
   - Identify the workflow purpose, trigger/source, target users, agent responsibilities, stop conditions, and handoff state.
   - Ask concise follow-up questions when required facts are missing. Do not invent tracker states, repository paths, credentials, or approval policy.
   - Capture acceptance criteria for the workflow itself and for each generated task.

2. Pseudocode
   - Write the intended dispatch loop in plain steps: fetch candidates, filter eligibility, prepare workspace, render prompt, run agent, validate output, publish handoff.
   - Include retry, continuation, blocked-task, and human-input paths.

3. Architecture
   - Draft the workflow front matter: task_source, polling, workspace, hooks, agent, and codex sections.
   - Draft the prompt body with the exact normalized task variables it needs, such as {{ issue.identifier }}, {{ issue.title }}, {{ issue.body }}, {{ issue.labels }}, and {{ attempt }}.
   - Name all external dependencies and environment variables.

4. Refinement
   - Check the draft against current Symphony constraints: strict template rendering, repo-owned workflow Markdown, normalized task records, bounded concurrency, and workspace lifecycle hooks.
   - Surface risks, invalid config, missing source adapters, or ambiguous handoff rules.

5. Completion
   - Produce a final `SPEC.md` section.
   - Produce the proposed `.symphony/workflows/<slug>.md` content.
   - Produce seed task records for `.symphony/tasks/<slug>.md` when the source is markdown-backed.
   - Produce verification commands: `vulcan symphony validate`, `vulcan symphony tasks`, and `vulcan symphony tick`.

Rules:
- Keep tracker-specific policy in the workflow prompt, not hidden operator state.
- Prefer markdown task sources for local prototypes unless the user explicitly chooses another supported source.
- Do not write files until the specification is complete and the user approves the generated workflow.
- If the user asks to implement immediately, still create the spec first, then implement from that spec.

