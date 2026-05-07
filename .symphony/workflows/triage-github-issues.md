---
task_source:
  kind: markdown
  markdown:
    path: ../tasks/triage-github-issues.md
polling:
  active_states: ["ready-for-agent"]
  max_concurrent: 1
workspace:
  root: ../workspaces
codex:
  command: codex
  args: ["--profile", "symphony"]
---
You are triaging GitHub issues for {{ issue.identifier }}: {{ issue.title }}.

## Instructions

1. **Fetch candidates:**
   Run `gh issue list --label "needs-triage" --repo yycholla/vulcan --json number,title,body,labels,state,createdAt --limit 20`

2. **Triage each issue one at a time:**

   a. Read the issue body and comments via `gh issue view <number> --repo yycholla/vulcan`

   b. **Classify** — apply a **category label**:
      - `bug` — something is broken or behaving incorrectly
      - `enhancement` — new feature or improvement request

   c. **Set a state label** — choose one:
      - `ready-for-agent` — fully specified, an agent can implement it with no further human context
      - `ready-for-human` — needs human judgment, design decisions, or external access
      - `needs-info` — not enough detail; tag the reporter with specific questions
      - `wontfix` — will not be actioned

   d. **Apply labels via CLI:**
      ```bash
      gh issue edit <number> --add-label "<category>" --repo yycholla/vulcan
      gh issue edit <number> --add-label "<state>" --repo yycholla/vulcan
      gh issue edit <number> --remove-label "needs-triage" --repo yycholla/vulcan
      ```

3. **Autonomous vs. escalate:**
   - **Classify confidently** whenever the issue is clear (obvious bug, obvious feature request, obvious wontfix).
   - **Escalate to the user** when unsure about classification, state, or the issue is too vague.
   - For escalated issues, present your reasoning and proposed action, then wait for a response.

4. **Write task files for ready-for-agent issues:**
   If you marked an issue `ready-for-agent`, append a new task record to `../tasks/forgehand-implementer.md` with the full spec in the body:

   ```yaml
   ---
   id: gh-<number>
   identifier: gh-<number>
   title: <issue title>
   state: ready-for-agent
   labels: [<category>]
   body: |
     <full spec derived from the issue body and any triage clarifications>
     ...
   ---
   ```

5. **Report** — summarise the results at the end showing what was triaged, what labels were applied, and what task files were created.

## Rules
- Do NOT modify unrelated issues.
- Do NOT close issues unless marked wontfix.
- If unsure, ask the user — do not guess.
