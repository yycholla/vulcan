---
id: triage-seed
identifier: triage-github-issues-seed
title: Triage GitHub issues with needs-triage label
state: ready-for-agent
labels: [symphony, triage]
body: |
  Fetch GitHub issues in yycholla/vulcan that have the needs-triage label,
  classify each one (bug/enhancement), apply appropriate labels and state,
  and write task files for any that are ready-for-agent.

  Only run this when the user requests it — it requires human escalation
  for uncertain cases.
---
