---
title: CONCERNS
last_mapped_commit: f5e07ca31dd7a4ac794b9838884bfdad74a7e37c
mapped_at: 2026-05-03
scope: full repo
---

# Codebase Concerns

**Analysis Date:** 2026-05-03

## Tech Debt

**Daemon session recovery and history APIs:**
- Issue: `session.search`, `session.resume`, and `session.history` return `METHOD_NOT_IMPLEMENTED`, and `session.create` accepts `resume_from` while ignoring it.
- Files: `src/daemon/handlers/session.rs`, `src/gateway/commands.rs`, `src/client/mod.rs`, `src/daemon/session.rs`
- Impact: Frontends can create and destroy live sessions, but cannot reliably discover or rehydrate durable session bodies through the daemon contract. Gateway `/resume` reports that daemon-backed resume is unsupported.
- Fix approach: Implement daemon-owned session history loading through the existing session store, wire `resume_from` into `SessionState` creation, then update gateway `/resume` to call the implemented path.

**Approval RPC surface is stubbed:**
- Issue: `approval.pending` and `approval.respond` are present in the daemon namespace but return `METHOD_NOT_IMPLEMENTED`.
- Files: `src/daemon/handlers/approval.rs`, `src/hooks/approval.rs`, `src/pause.rs`, `src/tui/pause_prompt.rs`
- Impact: The hook approval model exists, but external frontends cannot drive approval state through daemon RPC. This is a blocker for remote/gateway approval UX and can leave approval logic coupled to in-process TUI paths.
- Fix approach: Add a daemon-side approval queue keyed by session and turn, expose pending/respond handlers, and cover both TUI and gateway flows with daemon IPC tests.

**Replay execution modes are inspect-only:**
- Issue: `ReplayMode::Mock`, `ReplayMode::ToolReplay`, and `ReplayMode::Live` return typed "not yet implemented" errors.
- Files: `src/replay/mod.rs`, `src/cli_replay.rs`, `src/run_record/mod.rs`
- Impact: Saved run records can be inspected but not reproduced. Bugs involving provider/tool timing still need manual reproduction instead of deterministic replay.
- Fix approach: Implement mock replay first against recorded `RunRecord` events, then add tool replay behind the tool replay-safety metadata already present on tool definitions.

**Code intelligence graph is symbol-only:**
- Issue: The in-repo code graph explicitly defers call edges and impact-analysis relationships.
- Files: `src/code/graph.rs`, `src/tools/code_graph.rs`, `src/code/mod.rs`, `src/code/embed.rs`
- Impact: Local code navigation can find symbols, but impact analysis still depends on external GitNexus or manual exploration for caller/callee blast radius.
- Fix approach: Extend `CodeGraph::reindex` to persist call edges from tree-sitter captures, add migration coverage, and expose relationship queries through `src/tools/code_graph.rs`.

**Extension lifecycle is mostly metadata:**
- Issue: The extension registry and manifests capture capabilities, policy metadata, and audit events, while full daemon/frontend lifecycle, mid-session enable/disable, version matching, and frontend event routing remain documented foundation work.
- Files: `src/extensions/CONTEXT.md`, `src/extensions/registry.rs`, `src/extensions/manifest.rs`, `src/extensions/policy.rs`, `docs/adr/0003-extension-daemon-frontend-split.md`, `docs/adr/0004-extension-distribution-and-lifecycle.md`, `docs/adr/0007-extension-frontend-events-and-status-widgets.md`
- Impact: New extension work can easily add metadata paths without enforcing the runtime activation and capability boundaries promised by the ADRs.
- Fix approach: Keep new extension features behind manifest-declared capabilities, add lifecycle state transitions in `src/extensions/registry.rs`, and require daemon/frontend version compatibility tests.

**Clippy is informational in CI:**
- Issue: CI runs `cargo clippy --all-targets --all-features` with `continue-on-error: true`, and `Cargo.toml` still carries baseline allows for noisy rules.
- Files: `.github/workflows/ci.yml`, `Cargo.toml`
- Impact: Warning regressions can merge even though the workspace sets `RUSTFLAGS=-D warnings` for builds. Cleanup pressure is lower for large modules such as `src/tools/file.rs`, `src/tools/shell.rs`, `src/hooks/mod.rs`, and `src/tui/mod.rs`.
- Fix approach: Burn down current clippy output by area, remove `continue-on-error`, and keep per-site `#[allow(...)]` comments only where there is a concrete design reason.

**Documentation numbering drift:**
- Issue: ADR number `0007` is duplicated.
- Files: `docs/adr/0007-extension-frontend-events-and-status-widgets.md`, `docs/adr/0007-symphony-workflow-contract.md`
- Impact: Cross-references and future ADR insertion can become ambiguous.
- Fix approach: Renumber the Symphony ADR or add an ADR index that records stable aliases before more references accumulate.

## Known Bugs

**Gateway stream rendering is buffered instead of edit-in-place:**
- Symptoms: `process_one` drains only `text` frames into a final buffered reply and discards tool, reasoning, and non-text stream frames. Comments identify the streaming renderer bridge as deferred.
- Files: `src/gateway/worker.rs`, `src/gateway/stream_render.rs`, `src/gateway/outbound.rs`, `src/gateway/render_registry.rs`
- Trigger: Send a long gateway prompt or a prompt with tool/reasoning events through Discord, Telegram, or loopback.
- Workaround: Treat gateway replies as final buffered text until the daemon-to-gateway streaming render bridge lands.

**Outbound edit anchors can retry stale targets indefinitely:**
- Symptoms: Persistent platform edit failures for stale/deleted/too-old anchors are retried against the same anchor; first-send anchor capture can also happen before `mark_done`, allowing duplicate visible messages if the DB update fails.
- Files: `src/gateway/outbound.rs`, `src/gateway/render_registry.rs`, `src/gateway/queue.rs`
- Trigger: Platform edit fails permanently, or a crash/DB error occurs after `RenderRegistry::set_anchor` and before `OutboundQueue::mark_done`.
- Workaround: Operators can inspect logs and failed outbound rows, but there is no automatic fallback from edit to fresh send.

**Inbound heartbeat API is unused by workers:**
- Symptoms: `InboundQueue::heartbeat` exists, and `recover_processing` relies on `last_heartbeat_at`, but production gateway worker code does not call `heartbeat` during long-running prompt streams.
- Files: `src/gateway/queue.rs`, `src/gateway/worker.rs`, `src/gateway/mod.rs`
- Trigger: A prompt stream runs longer than `DEFAULT_INBOUND_HEARTBEAT_STALE_SECS` and the gateway restarts or recovery runs.
- Workaround: The default stale threshold is 30 minutes, which reduces normal exposure but does not protect legitimately long turns.

**Gateway resume command is a user-visible stub:**
- Symptoms: `/resume <session-id>` returns a clean message that daemon-backed resume is unsupported.
- Files: `src/gateway/commands.rs`, `src/daemon/handlers/session.rs`
- Trigger: A gateway user tries to resume an existing session from Discord, Telegram, or loopback.
- Workaround: Continue the active lane session or use non-gateway session tooling.

## Security Considerations

**File tool sandbox is deny-list based, not workspace-scoped:**
- Risk: File tools can still read and write arbitrary paths that are not in the sensitive-prefix deny list. The module explicitly defers workspace-root sandboxing.
- Files: `src/tools/fs_sandbox.rs`, `src/tools/file.rs`, `src/tools/profile.rs`, `src/config/mod.rs`
- Current mitigation: The deny list blocks high-risk pseudo-filesystems and common credential directories such as `/proc`, `/sys`, `/etc/shadow`, `~/.ssh`, `~/.aws`, `~/.kube`, `~/.docker`, `~/.vulcan`, and selected config directories.
- Recommendations: Add trust-profile scoped roots and per-session allowed directories, then make write tools default to workspace-only unless an explicit operator policy grants broader access.

**Gateway platform allowlists default open:**
- Risk: Discord guild/channel allowlists and Telegram chat allowlists use empty lists as "open", so a misconfigured enabled connector can serve every invited guild/channel/chat.
- Files: `src/config/mod.rs`, `src/gateway/discord.rs`, `src/gateway/telegram.rs`
- Current mitigation: Gateway bearer auth protects `/v1/*`, webhook auth is per-platform, and connector token validation rejects empty enabled bot tokens.
- Recommendations: For production configs, require non-empty allowlists when public connectors are enabled or add a startup warning that names the open connector.

**Custom gateway slash commands execute operator-configured processes:**
- Risk: `CommandConfig::Shell` executes configured commands under the gateway daemon user and passes inbound text through stdin. This is safer than shell interpolation but still grants platform users whatever behavior the configured executable implements.
- Files: `src/config/mod.rs`, `src/gateway/commands.rs`
- Current mitigation: Commands use `TokioCommand::new` without shell expansion, have a timeout, cap stdout, and expose only platform/chat/user metadata in environment variables.
- Recommendations: Add command allowlist validation, working-directory sandbox checks, stderr redaction, and per-command audience allowlists before encouraging production use.

**Web fetch SSRF check has a DNS-to-connect race:**
- Risk: URL validation resolves hostnames before fetch, but a hostile DNS target can theoretically change answers between validation and the underlying request.
- Files: `src/tools/web_ssrf.rs`, `src/tools/web.rs`
- Current mitigation: Validation blocks non-HTTP schemes, literal private/loopback/link-local/multicast/reserved addresses, and any private address returned during resolution.
- Recommendations: Use a connector that pins the validated IP for the actual request or resolves through a hardened HTTP client layer that re-checks the connected peer address.

**Provider wire debug depends on pattern redaction:**
- Risk: Wire-debug logs can include provider request/response bodies. Redaction covers recognizable secret shapes, but any unrecognized credential format can still leak.
- Files: `src/provider/openai.rs`, `src/provider/redact.rs`, `src/config/mod.rs`
- Current mitigation: `redact_value` and `redact_response_text` strip common bearer tokens, `sk-*` keys, GitHub PATs, AWS key IDs, Google keys, Slack tokens, Stripe keys, and generic JSON/env secret fields.
- Recommendations: Keep wire logging disabled by default, add structured deny-by-field redaction for all provider/tool debug values, and extend fixtures when new integrations add token formats.

## Performance Bottlenecks

**Outbound queue uses synchronous SQLite on async tasks:**
- Problem: `InboundQueue` routes rusqlite work through `spawn_blocking`, but `OutboundQueue` still checks out pooled connections and executes queries directly inside async methods.
- Files: `src/gateway/queue.rs`, `src/gateway/outbound.rs`
- Cause: Only inbound queue methods use the `db_blocking` helper.
- Improvement path: Move `OutboundQueue::enqueue`, `claim_due`, `mark_done`, `mark_failed`, `recover_sending`, and `peek` onto the same `db_blocking` pattern.

**Large modules concentrate hot-path complexity:**
- Problem: Several production files exceed 900 lines and mix core behavior, helpers, and tests.
- Files: `src/hooks/mod.rs`, `src/hooks/safety.rs`, `src/tools/file.rs`, `src/agent/run.rs`, `src/provider/openai.rs`, `src/tools/shell.rs`, `src/tui/mod.rs`, `src/tui/state/mod.rs`, `src/tui/views.rs`, `src/gateway/queue.rs`
- Cause: Foundation surfaces accumulated feature slices before being split into narrower modules.
- Improvement path: Extract stable submodules around protocol/state boundaries before adding new features; keep tests near the extracted modules.

**Gateway polling loops are fixed interval:**
- Problem: Outbound dispatch polls every 250 ms and drains due rows in a loop. Worker and scheduler paths also rely on polling-style loops.
- Files: `src/gateway/outbound.rs`, `src/gateway/mod.rs`, `src/gateway/scheduler.rs`
- Cause: SQLite-backed queue wakeups are not event-driven.
- Improvement path: Add notify-on-enqueue signals inside the process and keep timed polling as a crash-recovery fallback.

**Provider timeout is broad and per-request:**
- Problem: The OpenAI-compatible HTTP client uses a 300 second request timeout for all provider calls.
- Files: `src/provider/openai.rs`, `src/config/mod.rs`
- Cause: One timeout has to cover slow streaming, transient network stalls, and large model responses.
- Improvement path: Split connect/read/stream idle timeouts and expose provider-profile overrides.

## Fragile Areas

**Hook event ordering and mutation semantics:**
- Files: `src/hooks/mod.rs`, `src/hooks/approval.rs`, `src/hooks/safety.rs`, `src/hooks/skills.rs`, `src/agent/run.rs`, `src/agent/dispatch.rs`
- Why fragile: Blocking events use first non-continue wins while injections accumulate. Tool calls can have arguments or results replaced, and provider paths must keep buffered and streaming behavior aligned.
- Safe modification: Add tests for both buffered and streaming turns whenever a hook event, outcome, or built-in hook changes.
- Test coverage: `src/hooks/mod.rs` has extensive unit coverage, and `tests/agent_loop.rs` covers integration paths, but new event variants need explicit dual-path coverage.

**Daemon session lifecycle and cancellation:**
- Files: `src/daemon/session.rs`, `src/daemon/handlers/prompt.rs`, `src/daemon/handlers/session.rs`, `src/daemon/eviction.rs`, `src/daemon/config_watch.rs`
- Why fragile: Session state combines parking_lot locks, tokio mutexes, per-turn cancellation tokens, in-flight flags, idle eviction, lazy agent construction, and config reload deferral.
- Safe modification: Preserve single-flight semantics and update the session cancel token when a turn starts.
- Test coverage: `src/daemon/session.rs`, `src/daemon/lifecycle_tests.rs`, `tests/daemon_e2e.rs`, and `tests/client_autostart.rs` cover pieces, but resume/history paths are missing because the APIs are stubs.

**Gateway daemon-client routing:**
- Files: `src/gateway/worker.rs`, `src/gateway/lane_router.rs`, `src/gateway/daemon_client.rs`, `src/client/transport.rs`, `src/daemon/server.rs`
- Why fragile: One shared daemon client demultiplexes stream frames and normal responses while lane routing maps platform chat IDs to daemon sessions.
- Safe modification: Do not create per-row daemon clients; route all gateway prompt calls through `GatewayDaemonClient::shared_client` and `DaemonLaneRouter::ensure_session`.
- Test coverage: Some worker tests verify client reuse and slash-command routing, but multiple end-to-end gateway prompt tests are ignored until a daemon harness with scripted provider injection exists.

**Provider stream parsing and tool-call inference:**
- Files: `src/provider/openai.rs`, `src/provider/think_sanitizer.rs`, `src/provider/redact.rs`, `src/agent/provider.rs`
- Why fragile: OpenAI-compatible providers differ in SSE chunk shape, reasoning fields, content-embedded tool calls, retry headers, and error bodies.
- Safe modification: Add provider-specific fixtures for every new compatibility rule and keep redaction tests paired with logging changes.
- Test coverage: Unit tests cover many parser/redaction cases in `src/provider/openai.rs`, `src/provider/think_sanitizer.rs`, and `src/provider/redact.rs`; live provider behavior remains integration-risky.

**TUI state and rendering:**
- Files: `src/tui/mod.rs`, `src/tui/state/mod.rs`, `src/tui/rendering.rs`, `src/tui/chat_render.rs`, `src/tui/ui_runtime.rs`, `src/tui/views.rs`
- Why fragile: The TUI owns session UI state, queues/deferred commands, stream rendering, pause prompts, frontend extensions, and terminal layout constraints.
- Safe modification: Route behavioral changes through state methods and add focused snapshot/state tests before changing render layout or slash-command behavior.
- Test coverage: `src/tui/state/tests.rs`, `src/tui/chat_render.rs`, and `src/tui/rendering.rs` contain coverage, but visual regressions still need terminal-sized render tests for new views.

## Scaling Limits

**PTY shell sessions:**
- Current capacity: `MAX_PTY_SESSIONS` is 16 live PTY sessions per tool registry, each with a 64 KiB output buffer and 30 minute idle timeout.
- Limit: A single busy agent can hold many child shells, reader threads, and PTY resources until explicit close or idle reaping.
- Scaling path: Make the cap configurable per profile and expose PTY resource usage through diagnostics.

**Gateway lanes and queues:**
- Current capacity: Gateway defaults to `max_concurrent_lanes = 16`, inbound retry cap 3, outbound retry cap 5, and a 30 minute inbound heartbeat stale threshold.
- Limit: High-volume platforms can accumulate durable queue rows faster than fixed polling and lane workers drain them, especially when provider calls are slow.
- Scaling path: Add queue depth metrics, event-driven wakeups, per-platform worker caps, and periodic heartbeat during active prompt streams.

**Context and output token budgets:**
- Current capacity: Default provider max output is 8096 tokens, and configured context defaults live in `src/config/mod.rs`.
- Limit: Large context packs, long transcripts, and tool outputs can pressure provider context and log/render memory.
- Scaling path: Keep compaction in the turn runner, enforce tool-output caps at every tool boundary, and add tests for provider message validity after cancellation/compaction.

## Dependencies at Risk

**Gateway connector dependency surface:**
- Risk: Optional gateway features pull in Axum, Serenity, Telegram HTTP polling/webhooks, SQLite queues, and scheduler support.
- Impact: Feature interactions can break non-gateway builds or inflate compile time.
- Migration plan: Keep connector-specific code behind feature flags and run both default and `--features gateway` test/build paths in CI.

**Tree-sitter and LSP toolchain availability:**
- Risk: Code intelligence depends on parser crates and external language server binaries such as `rust-analyzer`, `typescript-language-server`, `pyright-langserver`, and `gopls`.
- Impact: Navigation tools fail or degrade based on local machine setup.
- Migration plan: Keep typed "not installed/not ready" errors, expose `doctor` checks, and avoid hard dependencies on external LSPs for core agent operation.

**OpenAI-compatible provider diversity:**
- Risk: The provider layer targets OpenAI-compatible APIs across OpenAI, OpenRouter, Anthropic-compatible proxies, Ollama, and local endpoints.
- Impact: Minor provider format changes can break stream parsing, tool calls, pricing/catalog metadata, or retry handling.
- Migration plan: Add provider catalog fixtures and compatibility tests for every supported provider profile shape.

## Missing Critical Features

**Daemon-backed session search/resume/history:**
- Problem: Users cannot search, resume, or inspect historical sessions through the daemon API.
- Blocks: Gateway `/resume`, multi-frontend session recovery, and durable session UX.

**Daemon approval queue:**
- Problem: Approval RPCs are unavailable.
- Blocks: Remote approval flows, gateway-mediated tool approvals, and frontend-independent pause/response handling.

**Gateway streaming-render bridge:**
- Problem: Gateway workers buffer final text and ignore non-text frames.
- Blocks: Discord/Telegram edit-in-place updates, tool card visibility, reasoning visibility, and richer connector UX.

**Replay execution:**
- Problem: Replay cannot re-run with mock provider/tool outputs.
- Blocks: Deterministic bug reproduction and regression tests from saved runs.

## Test Coverage Gaps

**Daemon-driven gateway prompt harness:**
- What's not tested: End-to-end HTTP inbound -> daemon prompt stream -> outbound delivery with a scripted provider is ignored.
- Files: `src/gateway/worker.rs`, `src/gateway/mod.rs`
- Risk: Gateway/daemon integration regressions can pass unit tests.
- Priority: High

**Session history and resume:**
- What's not tested: Search, resume, and history RPC success paths.
- Files: `src/daemon/handlers/session.rs`, `src/daemon/session.rs`, `src/client/mod.rs`
- Risk: Future implementations can drift from frontend expectations without acceptance tests.
- Priority: High

**Outbound queue async blocking behavior:**
- What's not tested: Runtime responsiveness under outbound queue load.
- Files: `src/gateway/queue.rs`, `src/gateway/outbound.rs`
- Risk: Synchronous SQLite work can block async workers under bursty outbound delivery.
- Priority: Medium

**Filesystem workspace sandboxing:**
- What's not tested: Profile-scoped root allowlists because workspace-root sandboxing is not implemented.
- Files: `src/tools/fs_sandbox.rs`, `src/tools/file.rs`, `src/tools/profile.rs`
- Risk: Prompt injection can still read/write non-denylisted paths outside the workspace when tools are available.
- Priority: High

**Visual TUI regression coverage:**
- What's not tested: Full terminal snapshots for new view/layout combinations across narrow and wide sizes.
- Files: `src/tui/rendering.rs`, `src/tui/views.rs`, `src/tui/chat_render.rs`, `src/tui/ui_runtime.rs`
- Risk: Layout, wrapping, and focus regressions can ship despite state-level tests.
- Priority: Medium

---

*Concerns audit: 2026-05-03*
