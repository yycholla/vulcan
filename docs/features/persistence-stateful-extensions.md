---
title: Persistence & Stateful Extensions
type: feature
created: 2026-05-14
tags: [extensions, persistence, state, multi-session]
---

# Persistence & Stateful Extensions

Extensions that preserve meaningful state across sessions and offer long-lived memory, plans, and auto-resume.

## Session Persistence

- **Save/restore agent state**: Serialize and restore active context, goals, partial results, and tool state so an agent can resume mid-task after restart.
- **Checkpointing**: Periodic snapshots during long-running tasks; recover from crashes with minimal loss.

## Knowledge Graph Extensions

Maintain an evolving, cross-session knowledge graph linking entities, tasks, and outcomes.

- **Entity resolution**: Merge mentions of the same entity across sessions.
- **Temporal reasoning**: Track changes over time and surface trends.
- **Querying**: Extensions can expose tools like `graph.query(...)` for agents to retrieve structured knowledge.

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
