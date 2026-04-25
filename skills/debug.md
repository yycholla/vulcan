---
name: debug
description: Systematic 4-phase debugging workflow
triggers: ["debug this", "bug", "error", "doesn't work", "failing"]
---

## Debugging Workflow

When asked to debug an issue, follow these phases:

### Phase 1 — Reproduce
1. Get the exact error message or failure condition
2. Create a minimal reproduction case
3. Confirm the issue is consistent

### Phase 2 — Root Cause
1. Read the relevant source files
2. Trace the data/code flow
3. Identify the specific line(s) where the behavior diverges from expectations
4. Check for common patterns: null values, off-by-one, type mismatches, race conditions

### Phase 3 — Fix
1. Apply the minimal change that addresses the root cause
2. Verify the fix doesn't break existing behavior
3. Add a regression test if appropriate

### Phase 4 — Verify
1. Run the reproduction case — it should pass now
2. Run any existing test suite
3. Summarize: what caused it, what was the fix, how to prevent recurrence
