---
name: symphony-workflow-setup
description: >
  Guides agents through human-in-the-loop Symphony workflow setup. Use when creating or changing
  Symphony workflows, workflow specs, task-source policy, approval gates, or handoff rules. Do not
  use for running existing workflows, listing tasks, or unrelated CLI scaffolding.
---

# Symphony Workflow Setup

Guide a human operator from rough intent to a complete Symphony workflow spec, then create workflow
files only after explicit approval.

## Core Rule

Keep the human in the loop. Do not write or edit workflow files until the user has approved a
complete spec that names the source, states, handoffs, approval policy, workspace behavior, and
verification commands.

## Process

1. Specification
   - Identify workflow purpose, trigger/source, target users, agent responsibilities, stop
     conditions, escalation points, and handoff state.
   - Ask concise follow-up questions for missing facts. Do not invent tracker states, repo paths,
     credentials, approval gates, or handoff rules.
   - Capture acceptance criteria for the workflow and for seed tasks.

2. Pseudocode
   - Write the intended loop: fetch candidates, filter eligibility, prepare workspace, render
     prompt, run agent, request human input when blocked, validate output, publish handoff.
   - Include retry, continuation, blocked-task, stale-workspace, and human-decision paths.

3. Architecture
   - Draft `task_source`, `polling`, `workspace`, `hooks`, `agent`, and `codex` front matter.
   - Draft prompt body using only supported normalized task variables:
     `{{ issue.identifier }}`, `{{ issue.title }}`, `{{ issue.body }}`, `{{ issue.labels }}`,
     `{{ issue.blockers }}`, `{{ issue.url }}`, and `{{ attempt }}`.
   - Name external dependencies, env vars, local commands, and approval assumptions.

4. Refinement
   - Check against Symphony constraints: repo-owned workflow Markdown, strict template rendering,
     normalized task records, bounded concurrency, workspace lifecycle hooks, and deterministic
     validation commands.
   - Surface risks, missing source adapters, invalid config, unsafe automation, or ambiguous human
     handoffs.

5. Completion
   - Present a final `SPEC.md` section.
   - Present proposed `.symphony/workflows/<slug>.md` content.
   - Present seed `.symphony/tasks/<slug>.md` records when the source is markdown-backed.
   - Present verification commands:
     `vulcan symphony validate <workflow>`,
     `vulcan symphony tasks <workflow>`,
     `vulcan symphony tick <workflow>`.
   - Ask for explicit approval before writing files.

## Output Shape

Use this structure:

```markdown
## Symphony Workflow Spec

### Purpose
...

### Human Gates
...

### Runtime Contract
...

### Prompt Contract
...

### Files To Write
...

### Verification
...
```
