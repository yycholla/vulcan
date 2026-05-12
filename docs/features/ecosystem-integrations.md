---
title: Ecosystem & Integrations
type: feature
status: proposed
phase: Phase 3 planning spec
created: 2026-05-08
updated: 2026-05-08
tracking: GitHub #277; Linear YYC-176 ecosystem catalog reference from issue audit
tags: [extensions, integrations, observability, infra]
---

# Ecosystem & Integrations

## Status

| Field | Value |
|---|---|
| Status | Proposed Phase 3 spec |
| Current implementation state | none for most integrations: observability foundations exist, but first-party Slack/email/CI/secret/data-source extensions are proposed catalog entries |
| Tracking | GitHub #277; Linear YYC-176 ecosystem catalog reference from issue audit |
| Dependencies / non-goals | Extension package/store (#266), governance (#269), and state APIs (#270). This document does not claim the proposed behavior is currently available. |

> Language note: sections below describe the target design. Unless the status table explicitly calls out a shipped foundation, read capability statements as proposed behavior.


A proposed catalog of extensions that would connect Vulcan to the broader tools and platforms teams already use.

## CI/CD Extensions

- **GitHub Actions / GitLab CI**: Extensions that allow agents to open PRs, run checks, and comment on builds.
- **Vulcan CI plugin**: Run agents as part of CI to review code, plan migrations, or validate infrastructure.

## Notification Extensions

- **Slack / Discord / Teams**: Post agent summaries, approvals, and alerts to channels.
- **Email**: Daily digests, failed-run alerts, milestone summaries.
- **SMS / Pager**: Critical failure and escalation notifications.

## Secret Manager Extensions

- **Vault / AWS Secrets Manager / GCP Secret Manager / 1Password**: Secure retrieval and injection of secrets into tool calls (never exposed to LLM context).
- **OS keyring**: Fallback for local development.

## Cloud Resource & IaC Extensions

- **Terraform / CloudFormation planner+applier**: Generate plans, estimate costs, and optionally apply with policy checks.
- **K8s operator**: Monitor cluster state and perform safe rollouts or rollbacks via agent guidance.

## Data Source Extensions

- **SQL/NoSQL**: Query databases (read/write) with schema-aware tools and protection against accidental DROP.
- **Notion / Jira / Linear**: Sync tasks, update tickets, and extract docs for RAG.
- **BigQuery / Snowflake**: Analytical query extensions with result summarization.

## Observability Extensions

- **OpenTelemetry**: Export agent traces, tool spans, and extension metrics.
- **Prometheus exporter**: Per-agent/tool counters and histograms.
- **Datadog / New Relic**: Out-of-the-box dashboards for agent activity.

---

## Example: OpenTelemetry Extension

```rust
pub struct OtelExporter {
    tracer: opentelemetry_sdk::trace::Tracer,
}

impl Extension for OtelExporter {
    fn capabilities(&self) -> &[Capability] {
        &[Capability::EventHandler("tool_call".into()), Capability::EventHandler("agent_end".into())]
    }

    fn initialize(&self, ctx: &ExtensionContext) -> Result<()> {
        ctx.register_event_handler(|event| match event {
            Event::BeforeToolCall { tool, args } => {
                self.tracer.in_span("tool_call", |cx| { /* record attrs */ });
            }
            Event::AfterToolCall { tool, result } => {
                /* end span and record outcome */
            }
            _ => {}
        });
        Ok(())
    }
}
```
