# Agent Comparison Wiki

Created: 2026-07-06

This folder captures the Mercury/Hermes investigation against Vulcan. It is split so future work can cite a focused note instead of re-reading one long report.

## Files

- [Vulcan baseline](vulcan-baseline.md) - what Vulcan already has, based on current repo code and docs.
- [Hermes Agent](hermes-agent.md) - feature and architecture profile from primary Hermes docs.
- [Mercury Agent](mercury.md) - feature and architecture profile from `cosmicstack-labs/mercury-agent`.
- [Comparison](comparison.md) - side-by-side gaps and lessons.
- [Recommendations](recommendations.md) - prioritized work Vulcan can borrow or avoid.
- [Sources](sources.md) - local and external references used.

## Bottom Line

Hermes is the useful agent comparison: broad platform gateway, explicit memory/skill loop, extensive tools, sandbox backends, cron, subagents, plugins, MCP, and editor integration.

Mercury Agent is now confirmed as a separate TypeScript/Node agent framework. It is smaller than Hermes but more directly comparable to Vulcan: permission-hardened tools, token budgets, CLI/Telegram/Web channels, daemon mode, scheduler, skills, subagents, Kanban boards, and SQLite-backed Second Brain memory.
