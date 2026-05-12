---
title: Persistence & Stateful Extensions
type: feature
status: proposed
phase: Phase 3 planning spec
created: 2026-05-08
updated: 2026-05-08
tracking: GitHub #270; Linear YYC-170 from issue audit
tags: [extensions, persistence, state, multi-session]
---

# Persistence & Stateful Extensions

## Status

| Field | Value |
|---|---|
| Status | Proposed Phase 3 spec |
| Current implementation state | foundation only: SQLite session persistence, FTS5 search, and extension state store handles exist; extension-owned checkpoints, knowledge graphs, and failover stores are proposed |
| Tracking | GitHub #270; Linear YYC-170 from issue audit |
| Dependencies / non-goals | Extension state store (#270), governance (#269), and runtime resource pool foundations. This document does not claim the proposed behavior is currently available. |

> Language note: sections below describe the target design. Unless the status table explicitly calls out a shipped foundation, read capability statements as proposed behavior.


Proposed extensions would preserve meaningful state across sessions and offer long-lived memory, plans, and auto-resume on top of shipped session persistence foundations.

## Session Persistence

- **Save/restore agent state**: Serialize and restore active context, goals, partial results, and tool state so an agent can resume mid-task after restart.
- **Checkpointing**: Periodic snapshots during long-running tasks; recover from crashes with minimal loss.

## Knowledge Graph Extensions

Maintain an evolving, cross-session knowledge graph linking entities, tasks, and outcomes.

- **Entity resolution**: Merge mentions of the same entity across sessions.
- **Temporal reasoning**: Track changes over time and surface trends.
- **Querying**: Proposed extensions could expose tools like `graph.query(...)` for agents to retrieve structured knowledge.

## Long-Term Planning Extensions

- **Project roadmaps**: Persisted multi-session plans with milestones, blockers, and resource needs.
- **Plan updates**: As work progresses, update estimates and priorities; warn about scope creep and deadline risks.
- **Goal tracking**: Track high-level objectives across weeks/months and synthesize weekly summaries.

## Auto-Resume & Failover

- **Auto-resume**: After a crash or deploy, automatically restart and rehydrate agent state where it left off.
- **Failover state store**: Replicated state (Redis/Postgres) so another process can pick up a session if the original node fails.

---

## Example: Knowledge Graph Extension API

```rust
pub trait KnowledgeGraph {
    fn add_fact(&self, fact: Fact) -> Result<FactId>;
    fn query(&self, q: Query) -> Result<Vec<Entity>>;
    fn merge(&self, a: EntityId, b: EntityId) -> Result<EntityId>;
}

impl Extension for KgExtension {
    fn capabilities(&self) -> &[Capability] {
        &[Capability::ToolProvider("kg_query".into()), Capability::MemoryBackend("kg".into())]
    }

    fn initialize(&self, ctx: &ExtensionContext) -> Result<()> {
        ctx.register_tool("kg.query", Arc::new(KgQueryTool(self.clone())));
        ctx.register_memory_backend("kg", Arc::new(self.clone()));
        Ok(())
    }
}
```
