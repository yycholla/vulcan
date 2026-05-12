---
title: Operability and Trust Substrate Contracts
status: proposed
tracking: GitHub #279 / #695
updated: 2026-05-11
tags: [operability, trust, run-records, artifacts, policy, diagnostics]
---

# Operability and Trust Substrate Contracts

This document defines the shared contracts for the operability/trust substrate described by GitHub #279 and decomposed by #695. It is intentionally contract-first: implementation slices should reuse these names, field boundaries, and non-goals instead of inventing separate observability, artifact, trust, or replay models.

The first implementation must benefit CLI/TUI flows without requiring gateway mode. Gateway lanes, subagents, extension surfaces, and richer tool frontends should integrate with the same contracts after the core local records are stable.

## Non-goals and boundaries

- Do not replace the hook system. Hooks remain the runtime extension points; these contracts record and explain hook effects.
- Do not build an extension marketplace. Marketplace/discovery work may depend on these contracts, but does not live here.
- Do not persist raw secrets, complete environment snapshots, or unchecked tool payloads by default.
- Do not require gateway mode, platform connectors, or daemon delivery queues for the first implementation.
- Do not promise byte-for-byte replay of unsafe side effects. Replay is bounded by redacted inputs, provider drift, tool determinism, and explicit replay-safety metadata.

## Storage boundary model

| Boundary | Purpose | May contain | Must not contain by default |
|---|---|---|---|
| Session history | User-visible conversation continuity. | Messages, assistant text, tool summaries, stable references to run records and artifacts. | Raw secrets, full environment dumps, unbounded tool payloads. |
| Run record store | Durable per-turn operational trace. | Run ids, status, actor/session references, redacted timeline events, artifact references, policy decisions. | Raw provider keys, unrestricted env snapshots, opaque unchecked payload blobs. |
| Artifact store | Outputs that outlive chat messages. | Typed artifact metadata, content-addressed file references, MIME/type hints, provenance, retention policy. | Secret material unless the artifact is explicitly marked sensitive and stored behind an approved secure path. |
| Config/trust store | User and workspace policy inputs. | Named capability profiles, workspace trust profiles, approval defaults, doctor findings. | Provider credentials or local private data unrelated to policy/config health. |
| Diagnostic output | Human/debug health reports. | Categorized findings, severity, remediations, redacted evidence. | Secrets in command output, absolute private payload contents beyond what is needed to explain a finding. |

## Contract summary

| Contract | Primary surface | Later implementers |
|---|---|---|
| `RunId` | Stable turn/run identity. | CLI/TUI run records, gateway lane correlation, subagent parent/child links. |
| `RunRecord` | Durable run-row schema. | Saved turn inspection, replay, diagnostics, gateway observability. |
| `ExecutionTimelineEvent` | Ordered trace events for decisions and side effects. | Hooks, policy, approvals, tools, provider calls, subagent dispatch, gateway delivery. |
| `ArtifactMetadata` | Typed durable output metadata. | Tool results, generated files/media, frontend cards, extension outputs. |
| `CapabilityProfile` | Named tool/capability bundle. | CLI profile selection, policy simulation, extension/MCP capability exposure. |
| `WorkspaceTrustProfile` | Per-workspace trust and approval posture. | Repository-sensitive coding workflows, tool gates, policy dry-run. |
| `DoctorFinding` | Structured diagnostics. | `vulcan doctor`, `/doctor`, gateway health, support bundles. |
| `ReplayPlan` | Bounded replay/simulation request. | Saved-turn replay, policy dry-run, reproduction reports. |

## `RunId`

Stable identifier assigned to every agent turn that can produce runtime decisions, provider calls, tool calls, or durable artifacts.

Fields:

| Field | Type | Notes |
|---|---|---|
| `run_id` | string | Globally unique, stable after assignment. Recommended shape: time-sortable opaque id such as `run_<ulid>` rather than a database row id. |
| `session_id` | string | Existing conversation/session id when available. Gateway lanes map platform conversations to this value rather than replacing it. |
| `parent_run_id` | string? | Set for subagent runs, replay attempts, or resumed/forked turns. |
| `origin` | enum | `cli`, `tui`, `gateway`, `subagent`, `replay`, `test`. |
| `created_at` | timestamp | UTC creation time. |

Redaction/storage boundary:

- `RunId` values are safe to store in session history, run records, artifact metadata, logs, and gateway queues.
- A `RunId` is not an authentication token and must not encode user prompt text, platform ids, repo paths, or secret-bearing values.

## `RunRecord`

Durable row for the lifecycle of one agent turn or subagent execution.

Fields:

| Field | Type | Notes |
|---|---|---|
| `run_id` | `RunId` | Primary lookup key. |
| `session_id` | string? | Conversation/session association. |
| `parent_run_id` | string? | Parent turn/subagent/replay relation. |
| `origin` | enum | Mirrors `RunId.origin`. |
| `status` | enum | `started`, `running`, `completed`, `failed`, `cancelled`, `blocked`, `replayed`. |
| `started_at` / `ended_at` | timestamp | UTC; `ended_at` is null while active. |
| `model_ref` | object? | Provider name, model slug, and provider endpoint class; no API keys. |
| `workspace_ref` | object? | Redacted workspace identity: trust-profile id, repo fingerprint, branch/ref if safe. |
| `capability_profile_id` | string? | Tool/capability profile active for this run. |
| `trust_profile_id` | string? | Workspace trust profile active for this run. |
| `timeline_ref` | string | Pointer to ordered `ExecutionTimelineEvent` records. |
| `artifact_refs` | string[] | References to `ArtifactMetadata` records produced by the run. |
| `error` | object? | Redacted category, message, and retryability; no raw secret-bearing payloads. |
| `summary` | string? | Human-readable run summary. |

Redaction/storage boundary:

- Prompt and tool payloads should be summarized or hashed unless they are already part of normal session history or an explicitly approved artifact.
- Provider usage/accounting may be stored; credentials, auth headers, cookies, and full request headers must be redacted.
- Workspace paths should be normalized to a repo/workspace identity when possible. Absolute paths may appear only when already user-visible and non-sensitive.

## `ExecutionTimelineEvent`

Append-only, ordered event describing decisions and side effects that shaped a run.

Fields:

| Field | Type | Notes |
|---|---|---|
| `event_id` | string | Unique event id, stable within the run. |
| `run_id` | string | Owning `RunId`. |
| `seq` | integer | Monotonic order within the run. |
| `occurred_at` | timestamp | UTC event time. |
| `phase` | enum | `before_prompt`, `provider_request`, `provider_response`, `before_tool_call`, `after_tool_call`, `before_agent_end`, `approval`, `policy`, `artifact`, `gateway`, `subagent`, `error`. |
| `actor` | enum | `agent`, `hook`, `policy`, `approval`, `tool`, `provider`, `gateway`, `subagent`, `user`, `system`. |
| `name` | string | Hook name, tool name, provider operation, policy id, approval gate, or gateway connector name. |
| `input_ref` | object? | Redacted reference/hash/summary of input. |
| `output_ref` | object? | Redacted reference/hash/summary of output. |
| `decision` | enum? | `continue`, `block`, `replace_args`, `replace_result`, `inject_messages`, `force_continue`, `approved`, `denied`, `error`. |
| `duration_ms` | integer? | Optional elapsed time. |
| `error` | object? | Redacted error category/message. |
| `metadata` | object | Small typed details relevant to the phase. |

Phase-specific metadata:

- Hooks: hook handler id, event kind, outcome, injected message count, and replacement summaries; never persist injected message bodies outside session history unless approved.
- Policies: policy id, matched rule id, simulated/enforced mode, decision, and remediation hint.
- Approvals: approval id, requested capability, approved/denied/skipped outcome, approver class (`user`, `policy`, `cached`), and expiry if cached.
- Tools: tool name, capability labels, sandbox/workspace mode, args schema version, args hash/summary, result hash/summary, exit status if applicable, artifact refs.
- Provider calls: provider/model, request class, streaming/buffered mode, token usage when available, finish reason, retry count; no raw API keys or provider headers.
- Gateway: platform connector, lane id, inbound/outbound queue ids, delivery status; no raw platform tokens.
- Subagents: child `run_id`, task label, capability profile id, workspace/trust refs.

## `ArtifactMetadata`

Typed metadata for an output that outlives a chat message.

Fields:

| Field | Type | Notes |
|---|---|---|
| `artifact_id` | string | Stable id, recommended `art_<ulid>`. |
| `run_id` | string | Producing run. |
| `kind` | enum | `text`, `patch`, `file`, `image`, `audio`, `video`, `table`, `json`, `log`, `report`, `plan`, `other`. |
| `title` | string? | Human label. |
| `mime_type` | string? | MIME type where applicable. |
| `schema` | string? | Schema id/version for structured artifacts. |
| `storage_uri` | string | Local file/content-addressed/object-store pointer. |
| `content_hash` | string? | Hash of persisted content when safe. |
| `size_bytes` | integer? | Stored size. |
| `created_at` | timestamp | UTC. |
| `provenance` | object | Producing tool/provider/hook and source input refs. |
| `visibility` | enum | `conversation`, `workspace`, `private`, `sensitive`. |
| `retention` | enum | `session`, `workspace`, `manual`, `ephemeral`. |
| `replay_safety` | enum | `safe`, `summary_only`, `unsafe`, `unknown`. |

Redaction/storage boundary:

- Artifact metadata is durable and inspectable; artifact content follows the artifact store's retention and visibility rules.
- Producers should prefer metadata-only creation from payload bytes: persist `content_hash`, `size_bytes`, `storage_uri`, type/schema/provenance, and redaction labels without copying raw payload bytes into the run timeline or CLI/TUI metadata.
- A tool may emit an artifact ref without exposing full content to the LLM-facing transcript.
- Sensitive artifacts require explicit classification and must not be copied into run timeline metadata.

First implementation notes:

- CLI/TUI surfaces should treat `RunEvent::ArtifactCreated` and `ArtifactMetadata` rows as artifact references, not normal assistant text. Rich rendering can layer on top of `kind`, `mime_type`, and `schema` later.
- Gateway delivery lanes should eventually translate artifact refs into platform-specific attachments/cards without requiring gateway mode for local artifact creation.
- Extension and MCP outputs should register artifact `kind`/`schema`/`replay_safety` when they start producing durable outputs, instead of adding extension-specific payload blobs to run records.

## `CapabilityProfile`

Named description of capabilities available to an agent/tool bundle.

Fields:

| Field | Type | Notes |
|---|---|---|
| `profile_id` | string | Stable local id, e.g. `default`, `coding-readonly`, `trusted-workspace`. |
| `display_name` | string | User-facing name. |
| `version` | integer/string | Increment when semantics change. |
| `tools` | object[] | Tool ids, command surfaces, schema versions, capability labels. |
| `permissions` | object | Filesystem, network, process, shell, browser, gateway, provider, and external-service permissions. |
| `approval_defaults` | object | Which capabilities require approval, may be auto-approved, or are denied. |
| `sandbox` | object | Workspace root, writable paths, env allowlist, timeout and output caps. |
| `extension_refs` | string[] | First-party/extension/MCP capabilities included by reference. |
| `policy_refs` | string[] | Policies evaluated for this profile. |
| `source` | enum | `builtin`, `config`, `workspace`, `extension`, `gateway`. |

Boundaries:

- Capability profiles describe what may be attempted; they do not store secrets needed to perform the attempt.
- Extension and MCP tools must be represented through the same profile shape before policy simulation or dry-run views can reason about them.

## `WorkspaceTrustProfile`

Per-workspace trust posture used by coding workflows, tool gates, and policy simulation.

Fields:

| Field | Type | Notes |
|---|---|---|
| `trust_profile_id` | string | Stable id for this workspace/repo trust posture. |
| `workspace_fingerprint` | object | Repo remote hash, root hash, or configured id; avoid leaking private path names by default. |
| `trust_level` | enum | `untrusted`, `limited`, `trusted`, `privileged`. |
| `allowed_roots` | string[] | Normalized roots the agent may read/write. |
| `write_policy` | enum/object | `deny`, `ask`, `workspace_only`, `allowlisted_paths`, `allow`. |
| `network_policy` | enum/object | `deny`, `ask`, `allowlisted_hosts`, `allow`. |
| `command_policy` | object | Shell/process policy, dangerous command rules, timeout caps. |
| `secret_policy` | object | Env/key redaction and commands/files considered secret-bearing. |
| `approval_policy` | object | When approvals are required or cached. |
| `audit_policy` | object | What timeline details are persisted and retention windows. |
| `source` | enum | `default`, `user_config`, `workspace_config`, `gateway_lane`, `test`. |

Boundaries:

- Trust profiles may reference environment variable names but must not store their values.
- Workspace trust should be available to CLI/TUI first and later reused by gateway lanes and subagents.

## Policy simulation and dry-run view

Use `vulcan policy simulate [path]` to inspect the currently effective policy for a workspace before starting a turn. The report includes the resolved workspace trust posture, effective capability profile, available/approval-gated/denied tools, configured hook summaries, and warning categories for broad capabilities such as shell, network, persistence, filesystem reads, and secret exposure.

Use proposed-value flags to compare a policy change before enabling it:

```bash
vulcan policy simulate /path/to/repo --profile readonly
vulcan policy simulate /path/to/repo --trust-level trusted --trust-profile coding
```

When a proposed profile or trust override is passed, the command prints a redacted dry-run delta (`current -> proposed`) followed by the proposed effective policy. Proposed values are not written to config, run records, or workspace trust rules by the simulator. Hook output intentionally lists only ids, events, match tools, enabled state, and policy; command paths, args, and environment values are not rendered.

Dependencies and future consumers:

- Extension execution and extension-pack policy should feed extension capabilities through the same `CapabilityProfile` shape before the simulator can reason about them.
- MCP tools should declare tool ids, capability labels, approval defaults, and redaction hints so they appear in policy deltas without MCP-specific payload storage.
- Subagent orchestration should pass parent/child capability and workspace trust refs into run records so dry-run output can explain inherited versus overridden policy.

## `DoctorFinding`

Structured diagnostic category used by `vulcan doctor`, `/doctor`, gateway health surfaces, and support bundles.

Fields:

| Field | Type | Notes |
|---|---|---|
| `finding_id` | string | Stable diagnostic id. |
| `category` | enum | `config`, `provider`, `storage`, `tool`, `gateway`. |
| `component` | string | Config key, provider id, store, tool name, gateway connector, etc. |
| `severity` | enum | `info`, `warning`, `error`, `critical`. |
| `status` | enum | `ok`, `degraded`, `failed`, `skipped`, `unknown`. |
| `message` | string | Concise human summary. |
| `evidence` | object | Redacted evidence: existence checks, version strings, status codes, path classes, queue depths. |
| `remediation` | string? | Concrete next step. |
| `run_id` | string? | Optional run that exposed the failure. |
| `created_at` | timestamp | UTC. |

Category contract:

- `config`: missing/invalid config, deprecated keys, config-file parse/load failures, incompatible options.
- `provider`: authentication reachability, model capability mismatch, timeout/rate-limit/transport failures, provider usage availability.
- `storage`: session/run/artifact/queue store open failures, migrations, permissions, corruption, disk capacity.
- `tool`: unavailable command/dependency, permission denial, sandbox violation, timeout, malformed schema/result.
- `gateway`: listener/auth/webhook/queue/platform connector health, lane routing, delivery failures.

## `ReplayPlan`

Contract for saved-turn replay and reproduction reports.

Fields:

| Field | Type | Notes |
|---|---|---|
| `replay_id` | string | Stable id for the replay attempt. |
| `source_run_id` | string | Run being replayed or simulated. |
| `mode` | enum | `inspect`, `simulate_policy`, `dry_run_tools`, `provider_replay`, `full_replay_best_effort`. |
| `inputs` | object | References to redacted session messages, timeline refs, artifact refs, capability/trust profile refs. |
| `limits` | object | Explicit exclusions and non-determinism notes. |
| `tool_strategy` | enum | `skip`, `summarize`, `dry_run`, `mock_from_record`, `rerun_allowed_safe_tools`. |
| `provider_strategy` | enum | `skip`, `reuse_recorded_summary`, `call_current_model`, `call_original_model_if_available`. |
| `output_run_id` | string? | New run id for replay output. |
| `findings` | `DoctorFinding[]` | Replay blockers or warnings. |

Replay/reproduction limits:

- `vulcan replay inspect <run-id>` is read-only timeline inspection.
- `vulcan replay simulate <run-id>` is the first CLI/TUI reproduction surface. It loads the durable run record, timeline events, artifacts for that run, and recorded trust/capability resolution, then prints a report with sections for reused context, redacted/unavailable inputs, missing artifacts, policy/capability mismatches, and reproduction limits.
- The current simulate mode does not call providers or execute tools. It treats provider metadata and tool fingerprints as evidence, not executable payloads.
- Replay can inspect recorded decisions, summaries, hashes, artifact refs, and redacted provider/tool metadata.
- Replay can simulate policy decisions against stored `CapabilityProfile` and `WorkspaceTrustProfile` inputs.
- Replay can dry-run safe tools that declare a dry-run mode and can mock results from recorded summaries/hashes.
- Replay must not reconstruct raw secrets, full env snapshots, private headers, or unchecked persisted tool payloads.
- Replay must mark provider output as non-deterministic unless an approved deterministic mock or recorded safe artifact is used.
- Replay must not rerun tools with side effects unless the tool declares replay safety and the active trust profile permits it.
- Gateway and subagent replay are dependencies for later slices: gateway lanes should attach platform delivery state to the same run ids, and subagent replay should recursively load child `RunId` records instead of flattening them into the parent report.

## Mapping to the seven #279 surfaces

| #279 surface | Contract coverage in this slice | Later implementation slice |
|---|---|---|
| 1. Durable run records and execution timelines | `RunId`, `RunRecord`, `ExecutionTimelineEvent`, storage/redaction boundaries. | Implement durable run records and execution timelines (`t_c527e877`). |
| 2. Typed artifacts for outputs that outlive chat messages | `ArtifactMetadata`, artifact store boundary, artifact refs on run/timeline events. | Implement typed artifacts (`t_d6c48925`). |
| 3. Tool capability profiles for named agent/tool bundles | `CapabilityProfile`, tool/extension/MCP capability fields, approval defaults. | Implement tool capability profiles and workspace trust profiles (`t_76749e3d`). |
| 4. Workspace trust profiles for per-repository safety policy | `WorkspaceTrustProfile`, workspace fingerprinting, read/write/network/command/secret policies. | Implement tool capability profiles and workspace trust profiles (`t_76749e3d`). |
| 5. `vulcan doctor` and `/doctor` diagnostics | `DoctorFinding`, diagnostic categories for config/provider/storage/tool/gateway failures. | Implement doctor and config-health diagnostics (`t_29acde78`). |
| 6. Replay and reproduction of saved turns | `ReplayPlan`, replay limits, tool/provider strategies, timeline/artifact inputs. | Implement saved-turn replay and reproduction limits (`t_4b9ba22a`). |
| 7. Policy simulation and dry-run views | `CapabilityProfile`, `WorkspaceTrustProfile`, `ExecutionTimelineEvent.policy`, `ReplayPlan.mode=simulate_policy`. | Implement policy simulation and dry-run inspection views (`t_370f9339`). |

## Cross-surface dependencies

- Gateway: gateway lanes should attach `RunId`/`session_id` correlation, queue ids, platform connector names, and delivery statuses to timeline events without making gateway mode mandatory for local records.
- Subagents: subagent dispatch should allocate child `RunId` values with `parent_run_id`, inherit or explicitly override capability/trust profiles, and emit parent/child timeline links.
- Extensions and MCP tools: extensions contribute capabilities through `CapabilityProfile.extension_refs` and tool metadata; marketplace/package lifecycle remains outside this slice.
- Tool surfaces: tools should declare capability labels, args/result schema versions, dry-run/replay-safety posture, artifact outputs, and redaction hints so run records do not need tool-specific ad hoc storage.
- Hooks: hook outcomes map directly to `ExecutionTimelineEvent.decision`; the hook system remains the source of behavior, while timeline events make behavior inspectable.
- Provider layer: provider calls contribute model refs, request class, streaming/buffered mode, usage, retries, and redacted errors. Provider credentials remain outside run records.
- Storage: session, run record, artifact, config/trust, and diagnostic stores may share a SQLite/root directory foundation, but their retention and redaction rules remain distinct.

## Integration status for non-CLI surfaces (#279/#695)

- Gateway lanes are wired through the same run-record origin contract where the daemon stream path is stable: `gateway::worker` includes an `origin.kind=gateway` envelope on `prompt.stream`, daemon dispatch converts it to `RunOrigin::Gateway { lane }`, and the agent stream wrapper records the turn under that origin. Gateway remains optional; local CLI/TUI run records do not depend on gateway queues.
- Subagents are wired through the same run-record and policy surface: daemon subagent runs call `run_prompt_with_cancel_origin(..., RunOrigin::Subagent { parent_run_id })`; `spawn_subagent` responses expose `capability_profile`, `policy_surface=tools.profiles`, `replay_safety=summary_only`, `tools_granted`, and budget usage instead of adding a parallel observability schema.
- Extension and MCP/tool surfaces remain CLI/TUI-first for execution, but their policy contracts are intentionally the same substrate: policy simulation consumes `ToolRegistry`/`ToolContext`, MCP tools implement the normal `Tool` replay-safety surface, and extension capabilities are modeled as `CapabilityProfile.extension_refs` rather than an extension marketplace policy model.
- Deferred follow-ups tied to #279 related work: gateway delivery-status timeline events, platform attachment/card rendering from artifact refs, extension-pack/MCP capability label ingestion, and recursive subagent replay. These are explicitly additive consumers of `RunRecord`, `ExecutionTimelineEvent`, `CapabilityProfile`, and `ReplayPlan`, not separate mechanisms.

## Final integration checklist (#695)

This checklist is the final #695 integration pass against the seven #279 surfaces and success criteria.

| #279 surface / success criterion | Status | Code/docs/tests evidence |
|---|---|---|
| 1. Durable run records and execution timelines; every agent turn has a stable run identifier. | Complete for CLI/TUI core paths, gateway origin stamping, and subagent origin stamping. | `src/run_record/mod.rs`, `src/agent/run.rs`, `src/cli_run.rs`, `src/daemon/dispatch.rs`, `src/daemon/handlers/prompt.rs`, `src/daemon/subagent.rs`; tests: `cargo test -q -p vulcan-core --lib run_record`, `run_prompt_with_cancel_origin_stamps_subagent_run_record`, `frontend_options_capture_gateway_run_origin`. |
| 2. Typed artifacts for outputs that outlive chat messages. | Complete for metadata creation/listing, run association, CLI inspection, and timeline artifact refs. | `src/artifact/mod.rs`, `src/cli_artifact.rs`, `src/runtime_pool.rs`, `src/run_record/mod.rs`; tests: `cargo test artifact --all-targets`; manual surface: `vulcan artifact ...`. |
| 3. Tool capability profiles for named agent/tool bundles. | Complete for profile loading/resolution, tool registry filtering, CLI/profile override, policy simulation, and run metadata. | `src/tools/profile.rs`, `src/tools/mod.rs`, `src/cli_policy.rs`, `src/policy/mod.rs`; tests: `cargo test -q -p vulcan-core --lib profile`, `cargo test -q -p vulcan-core --lib policy::tests`; manual check: `vulcan policy simulate /home/yycholla/vulcan --profile readonly`. |
| 4. Workspace trust profiles for per-repository safety policy. | Complete for workspace trust resolution, inspection, trust-derived capability profiles, redaction, and policy simulation. | `src/trust/mod.rs`, `src/cli_trust.rs`, `src/config/mod.rs`, `src/config_registry.rs`; tests: `cargo test -q -p vulcan-core --lib trust`, `cargo test -q -p vulcan-core --lib redacts`; manual check: `vulcan trust why /path/to/workspace`. |
| 5. `vulcan doctor` and `/doctor` diagnostics; diagnostics distinguish config/provider/storage/tool/gateway failures. | Complete for CLI diagnostics across config fragments, provider profile, storage, tool registry, gateway config, workspace trust, and capability profile checks. `/doctor` remains a TUI command-surface consumer of the same diagnostics category contract. | `src/doctor/mod.rs`; tests: `cargo test -q -p vulcan-core --lib doctor::tests`; manual check: `vulcan doctor` reports categorized pass/warn/fail findings with redacted evidence/remediations. |
| 6. Replay and reproduction of saved turns; failing turns can be replayed/simulated with clear limits. | Complete for non-executing saved-run replay simulation that loads run metadata, artifacts, trust/capability state, and explicitly reports redacted/unavailable inputs and reproduction limits. | `src/replay/mod.rs`, `src/cli_replay.rs`; tests: `cargo test -q -p vulcan-core --lib replay::tests`; manual check: `vulcan replay simulate <run-id>`. |
| 7. Policy simulation and dry-run views; users can inspect policy/tool changes before enabling them. | Complete for current-policy inspection and proposed profile/trust dry-run deltas without persistence. | `src/policy/mod.rs`, `src/cli_policy.rs`; tests: `cargo test -q -p vulcan-core --lib policy::tests`; manual check: `vulcan policy simulate /path --profile readonly` prints a redacted `current -> proposed` delta. |
| Important runtime decisions are inspectable without combing debug logs. | Complete for hooks, policies, approvals, provider calls, tool calls, artifacts, gateway origin, and subagent lineage where producers are currently wired. | `RunEvent` timeline records in `src/run_record/mod.rs` plus producer wiring in `src/agent/run.rs`, `src/tools/spawn.rs`, and daemon/gateway integration files. |
| Tool permissions can be reasoned about through named profiles, not ad hoc conditionals. | Complete for core `ToolRegistry` and policy simulation; future extension/MCP ingestion is a follow-up consumer of the same profile shape. | `ToolProfile`, `ToolRegistry::with_profile`, policy simulator output, and #697. |
| Extension and subagent work can build on this layer instead of inventing separate observability/policy mechanisms. | Complete for subagent lineage/capability metadata and documented for extensions/MCP; remaining consumers are tracked rather than hidden. | `spawn_subagent` returns `capability_profile`, `policy_surface=tools.profiles`, `replay_safety=summary_only`, `tools_granted`; follow-up #697 covers deferred gateway delivery events, platform artifact rendering, extension/MCP profile ingestion, and recursive subagent replay. |

### Integrated verification

Focused tests and manual checks exercised reproducibility, capability/trust inspection, and config health together after the decomposition slices landed:

- `cargo test -q -p vulcan-core --lib replay::tests && cargo test -q -p vulcan-core --lib policy::tests && cargo test -q -p vulcan-core --lib doctor::tests && cargo test -q -p vulcan-core --lib redacts` passed locally during the final pass.
- `vulcan replay simulate ac0430d2` produced a non-executing reproduction report with recorded model/trust metadata, redacted prompt body, and explicit limits: provider nondeterminism plus no reconstruction of raw secrets/full env/unchecked tool payloads.
- `vulcan policy simulate /home/yycholla/vulcan --profile readonly` produced a redacted dry-run delta and did not persist proposed profile/trust values.
- `vulcan doctor` produced categorized config/provider/storage/tool/gateway/workspace-trust/capability findings. The local environment currently reports an unsupported configured provider type (`openai-responses`) as a doctor failure, which verifies the config-health path is active rather than silently passing bad config.

### Redaction and non-persistence verification

- Run-record prompts/tool payloads store fingerprints, summaries, counts, and references by default, not raw prompt bodies, raw provider headers, secret values, full environment snapshots, or unchecked tool-result blobs.
- Artifact producers persist typed metadata and refs; sensitive content must be explicitly classified and is not copied into run timeline metadata.
- Policy dry-run output intentionally renders hook ids/events/match tools/enabled/policy metadata only; hook command paths, args, and env values are not rendered.
- Replay simulation reports redacted/unavailable inputs and never reconstructs raw secrets, full env snapshots, provider tokens, or unchecked payloads.
- Doctor output passes all findings through secret-like redaction for messages/remediations and reports provider credential source without printing credential values.

### Remaining follow-up

No hidden #695 blockers remain for the first implementation. Deferred non-CLI consumers are tracked in GitHub #697: gateway delivery-status timeline events, platform artifact cards from artifact refs, extension/MCP capability-label ingestion, and recursive subagent replay.

## Implementation guidance for follow-up slices

1. Add data types close to the eventual module boundaries (`run_record`, `artifact`, `policy`/`trust`, `doctor`, `replay`) without wiring behavior before the slice requires it.
2. Make redaction explicit in constructors or persistence boundaries; do not rely on downstream UI layers to remember what is safe.
3. Keep timeline payloads small and typed. Large content belongs in session history if conversational, artifact storage if durable, or nowhere if secret/unsafe.
4. Prefer schema/version fields on profiles, tool metadata, and artifacts so future extensions and gateway surfaces can evolve without breaking old records.
5. Treat every replay mode as best-effort unless all participating tools/providers declare deterministic, replay-safe behavior.
