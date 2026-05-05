---
title: First-Party Extension Catalog
type: reference
created: 2026-05-05
tags: [extensions, catalog, integrations, observability, rag]
---

# First-Party Extension Catalog

This catalog ranks extension candidates for validating Vulcan's extension
platform. It is intentionally conservative: first-party extensions should prove
the runtime, policy, audit, and user-facing inventory surfaces before Vulcan
invites broad third-party or marketplace integrations.

## Selection Rules

Good first candidates are small, observable, and reversible.

- They should use existing extension capabilities before requiring new runtime
  contracts.
- They should avoid secret material, write-capable infrastructure, SMS/pager
  escalation, and cloud apply paths.
- They should produce useful audit records and inventory metadata.
- They should have a clear MCP-vs-extension boundary so Vulcan does not wrap a
  remote tool server when a normal MCP server is enough.

## Ranked First-Party Candidates

| Rank | Candidate | Category | Integration class | Minimum behavior | Required capabilities | Policy posture | Why now |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Event Logger | Observability | First-party extension | Subscribe to hook/runtime events and write structured local audit rows or JSONL snapshots. | `lifecycle_observer`, `hook_handler` | Allow local file writes only under Vulcan home; no network. | Exercises audit surfaces without tool risk. |
| 2 | GitHub/CI Summary | CI/CD | MCP-backed or first-party extension | Read PR/check status and summarize failing jobs. | `tool_provider` or MCP tools, optional `prompt_injection` | Read-only GitHub scope; no comments, merges, or reruns. | Useful day-to-day and validates read-only external integration policy. |
| 3 | OpenTelemetry Exporter | Observability | First-party extension | Export agent/tool/extension spans to a configured OTLP endpoint. | `lifecycle_observer`, `hook_handler` | Network egress only to configured endpoint; redact prompt/tool payloads by default. | Builds operational feedback for the extension runtime itself. |
| 4 | Local Secret Resolver | Secret manager | First-party extension | Resolve named secrets from OS keyring for authorized tools without exposing values to model context. | `tool_provider`, future secret-handle capability | Require explicit allowlist and audit every resolution. | Establishes the safe pattern before Vault/cloud managers. |
| 5 | Read-Only SQL Explorer | Data source | First-party extension | Inspect schema and run bounded read-only queries. | `tool_provider` | Read-only DSN, query timeout, row limit, deny mutation keywords. | Validates structured data access with tight blast-radius controls. |
| 6 | RAG Ingestion Adapter | RAG | First-party extension | Ingest local docs/git paths into the existing memory/index path. | `memory_backend`, `lifecycle_observer` | Local paths only; no web crawling in first slice. | Tests long-running stateful extension behavior. |
| 7 | Model Router Policy | Model routing | First-party extension | Recommend provider/model by task class and budget metadata. | future routing hook, `prompt_injection` | Advisory mode first; no automatic rerouting until provider policy is stable. | Useful later, but depends on a firmer provider contract. |

## First Implementation Targets

### Event Logger

This should be the first first-party extension after the current runtime
foundation. It can run entirely locally and primarily observes existing events.

- Minimum behavior: capture extension activation, hook outcomes, tool call
  starts/ends, failures, and policy decisions as structured records.
- Required extension capabilities: `lifecycle_observer`, `hook_handler`.
- Data sensitivity: prompt/tool payload capture should be off by default; event
  metadata is enough for the first slice.
- Success signal: an operator can inspect recent extension activity without
  replaying logs or enabling global debug tracing.

### GitHub/CI Summary

This is the first useful external integration candidate, but it should stay
read-only until the approval and policy surfaces are stronger.

- Minimum behavior: summarize open PR status, failed checks, and recent CI
  failure snippets using GitHub APIs or an MCP-backed GitHub tool surface.
- Required extension capabilities: `tool_provider` if implemented natively, or
  MCP server configuration plus extension metadata if backed by MCP.
- Data sensitivity: repository metadata and CI logs may contain secrets; logs
  should be truncated and redacted before model injection.
- Success signal: the extension can answer "what is blocking this PR?" without
  commenting, merging, rerunning checks, or mutating GitHub state.

## Integration Boundaries

| Class | Use when | Examples | Initial trust model |
| --- | --- | --- | --- |
| First-party extension | Vulcan needs local lifecycle hooks, runtime state, policy decisions, or tight TUI/daemon integration. | Event logger, OTLP exporter, local secret resolver. | Bundled or explicitly installed, audited, version-pinned. |
| Third-party extension | A package contributes local behavior but is not maintained with Vulcan. | Team-specific policy engine, internal data adapter. | Repository metadata, checksum verification, explicit enablement. |
| MCP-backed integration | The integration is primarily a remote or subprocess tool server and does not need Vulcan lifecycle hooks. | GitHub read-only tools, Linear/Jira/Notion search, database query tool server. | MCP server policy plus extension/catalog metadata only when needed for discovery. |

## Deferred High-Risk Candidates

These are intentionally out of the first implementation batch.

- Write-capable databases and warehouses.
- Terraform, CloudFormation, Kubernetes apply, or cloud resource mutation.
- SMS, pager, incident escalation, or human wake-up paths.
- Secret injection into arbitrary tool calls.
- Paid marketplace, license enforcement, ratings, or reviews.
- Autonomous model routing that changes provider/model without explicit user
  policy.

## Capability Gaps To Track

- Secret-handle capability that lets tools receive opaque secret references
  without exposing raw values to prompt context.
- Network egress policy with per-extension endpoint allowlists.
- Payload redaction controls for observability and CI log extensions.
- Advisory model-routing hook before any automatic routing behavior.
- MCP-backed extension metadata so tool servers can appear in inventory without
  pretending to be local runtime extensions.
