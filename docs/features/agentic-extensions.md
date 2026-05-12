---
title: Agentic & AI-Native Extensions
type: feature
status: proposed
phase: Phase 3 planning spec
created: 2026-05-08
updated: 2026-05-08
tracking: GitHub #271; Linear YYC-82 / YYC-68 historical orchestration refs
tags: [extensions, ai, agents, orchestration]
---

# Agentic & AI-Native Extensions

## Status

| Field | Value |
|---|---|
| Status | Proposed Phase 3 spec |
| Current implementation state | foundation only: orchestration stores and subagent records exist; extension-controlled planning/delegation hooks are proposed Phase 3 behavior |
| Tracking | GitHub #271; Linear YYC-82 / YYC-68 historical orchestration refs |
| Dependencies / non-goals | Extension foundation (#265), governance/policy (#269), and the orchestration runtime epic (#53). This document does not claim the proposed behavior is currently available. |

> Language note: sections below describe the target design. Unless the status table explicitly calls out a shipped foundation, read capability statements as proposed behavior.


Make extensions not just tools, but autonomous collaborators that participate in multi-agent workflows and reasoning loops.

## Agent-as-Extension

Proposed extensions would be able to embed or spawn sub-agents that operate semi-autonomously within bounded contexts.

- **Background agents**: A proposed memory extension (`memory@redis`) could run a background agent that preloads relevant facts at session start and prunes stale entries after sessions end.
- **Specialist agents**: Proposed extensions could vend specialized agents (e.g., a SQL analyst agent, a log triage agent) that the main agent could delegate to via the orchestration layer.

## Planning & Orchestration Hooks

Allow extensions to observe, influence, and intercept planning and execution steps.

| Hook | When Fired | Use Cases |
|------|------------|-----------|
| `BeforePlan` | Before the agent generates a plan | Inject preconditions, add steps, block unsafe plans |
| `AfterPlan` | After plan generation, before execution | Annotate steps with metadata, estimate cost, warn on risky ops |
| `OnStepStart` | Before each plan step executes | Prepare context, pre-warm caches |
| `OnStepEnd` | After each step completes | Observe outcome, rewrite result, collect metrics |
| `OnDelegation` | When agent delegates to sub-agent | Approve, route, or reconfigure delegation |

## Multi-Agent Coordination

Extensions that mediate between agents during handoffs, consensus, or parallel execution.

- **Consensus mediator**: Extensions that collect answers from multiple agents and reconcile conflicts.
- **Role router**: Route requests to agents tagged with specific roles ("reviewer", "coder", "ops").
- **Handoff protocol**: Standardized state transfer (memory, task context, partial results) between agents.

## RAG Extensions

Pluggable retrieval-augmented generation pipelines implemented as extensions.

- **Ingestion adapters**: Watch folders, git repos, or web sources; chunk, embed, and store.
- **Chunking strategies**: Semantic, recursive, code-aware, or domain-specific chunkers.
- **Embedding adapters**: Support open-source and proprietary embedding models.
- **Vector store backends**: Chroma, Pinecone, Qdrant, pgvector, Redis — swap via extension.

## Model Router Extensions

Policy-driven selection of models/providers per task type.

- **Cost/latency policies**: Prefer fast/cheap for formatting; smart/expensive for reasoning.
- **Capability gating**: Route to models with strong tool-use or coding capabilities for those tasks.
- **Failover**: Automatic retry on alternative providers on error or rate-limit.

## Dynamic Tool Generation

Extensions that turn external APIs into first-class tools at runtime.

- **OpenAPI → Tool**: Given an API spec, a proposed extension could generate a typed tool the agent can call immediately.
- **Schema reflection**: Describe tool capabilities at runtime for UI rendering and planning.

---

## Example: Planning Interceptor Extension

```rust
pub struct PlanningGuard;

impl Extension for PlanningGuard {
    fn capabilities(&self) -> &[Capability] {
        &[Capability::EventHandler("planning".into())]
    }

    fn initialize(&self, ctx: &ExtensionContext) -> Result<()> {
        ctx.register_event_handler(|event| match event {
            Event::BeforePlan { plan, .. } => {
                if plan.steps.iter().any(|s| s.contains("rm -rf")) {
                    warn!("Dangerous operation detected; requiring approval");
                    block_until_approved();
                }
            }
            _ => {}
        });
        Ok(())
    }
}
```
