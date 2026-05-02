---
title: Governance, Safety & Policy
type: feature
created: 2026-05-14
tags: [extensions, safety, policy, audit]
---

# Governance, Safety & Policy

Strong, transparent controls for what agents and extensions are allowed to do.

## Policy Extensions

Pluggable policy engines enforce org-specific rules before actions execute.

- **OPA/Rego**: Define policies as code — allowed tools, allowed resources, time windows.
- **Custom policies**: Extensions implement `PolicyProvider` trait to integrate proprietary engines.
- **Escalation and overrides**: Time-bound exceptions, break-glass roles, and audit trails.

## Audit Logging Extensions

Capture full provenance for compliance and forensics.

- **Append-only logs**: Tamper-evident logs of prompts, tool calls, and results.
- **SIEM integration**: Stream to Splunk, ELK, Datadog, or similar.
- **Redaction**: Scrub secrets and PII at capture time (see Sensitive Data Scrubbing).

## Sensitive Data Scrubbing

Extensions that automatically mask secrets from prompts and tool results.

- Detect API keys, tokens, passwords via regex, entropy, or vault lookups.
- Replace with `[REDACTED]` in LLM context while preserving originals in secure memory for authorized tools.

## Resource Quotas & Budgets

Extensions enforce limits to prevent runaway costs or resource exhaustion.

- **Token budgets**: Cap total tokens per session/day/project.
- **Cost caps**: Enforce max spend per cloud provider or tool.
- **Kill switches**: Stop or pause agents that exceed quotas.
- **Throttling**: Rate-limit expensive tool use per agent or per org.

## Reputation & Trust Scoring

Help users choose safe extensions.

- **Automated scanning**: Static + behavioral analysis of WASM/native modules for malicious patterns.
- **Behavioral telemetry**: Flag extensions that perform unexpected syscalls or network calls.
- **Community ratings + publisher badges**: Curated publishers and transparent review history.

## Rollback & Version Pinning

Safe extension lifecycle management.

- Pin extensions to exact versions per environment (dev / staging / prod).
- One-click rollback to last-known-good config + binary.
- Snapshot configs and restore atomically.

---

## Example: Resource Quota Extension

```rust
pub struct QuotaEnforcer {
    limits: Limits,
    usage: Arc<UsageStore>,
}

impl Extension for QuotaEnforcer { /* ... */ }

// Hook into BeforeToolCall
if usage.projected_cost(&tool) > limits.remaining {
    block_and_notify("Quota exceeded");
}
```
