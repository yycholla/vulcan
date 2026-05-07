---
task_source:
  kind: markdown
  markdown:
    path: ../tasks/update-github-issues.md
polling:
  active_states: [ready-for-agent]
  max_concurrent: 1
workspace:
  root: ../workspaces
codex:
  command: codex
  args: ["--profile", "symphony"]
---
Handle {{ issue.identifier }}: {{ issue.title }}
