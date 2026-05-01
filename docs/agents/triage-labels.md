# Triage Labels

The skills speak in terms of five canonical triage roles. This file maps those roles to the actual label strings used in this repo's backlog.

| Label in mattpocock/skills | Label in our backlog | Meaning                                  |
| -------------------------- | -------------------- | ---------------------------------------- |
| `needs-triage`             | `needs-triage`       | Maintainer needs to evaluate this issue  |
| `needs-info`               | `needs-info`         | Waiting on reporter for more information |
| `ready-for-agent`          | `ready-for-agent`    | Fully specified, ready for an AFK agent  |
| `ready-for-human`          | `ready-for-human`    | Requires human implementation            |
| `wontfix`                  | `wontfix`            | Will not be actioned                     |

When a skill mentions a role (e.g. "apply the AFK-ready triage label"), use the corresponding label string from this table.

Edit the right-hand column to match whatever vocabulary you actually use.

## Setup

`wontfix` already exists. Create the other four:

```bash
gh label create needs-triage   -R yycholla/vulcan -c "#FBCA04" -d "Maintainer needs to evaluate"
gh label create needs-info     -R yycholla/vulcan -c "#D4C5F9" -d "Waiting on reporter"
gh label create ready-for-agent -R yycholla/vulcan -c "#0E8A16" -d "Fully specified, AFK-ready"
gh label create ready-for-human -R yycholla/vulcan -c "#1D76DB" -d "Needs human implementation"
```
