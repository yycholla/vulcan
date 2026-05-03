# Observability Runtime Metrics Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add first-class OpenTelemetry metrics for Vulcan runtime performance, including backend request/tool/provider/hook timing and TUI frame health.

**Architecture:** Keep traces for correlation and add low-cardinality metrics for dashboards and alerting. Split the work into independent PRs: shared metric helpers, backend/runtime metrics, TUI frame metrics, and process resource metrics.

**Tech Stack:** Rust, `opentelemetry`, `opentelemetry_sdk`, `tracing_opentelemetry`, Ratatui/TUI loop, optional `sysinfo` for process sampling.

---

### Task 1: Shared Metrics Vocabulary and Helpers

**Files:**
- Modify: `src/observability.rs`

**Step 1: Write the failing test**

Add an observability unit test that asserts stable metric names for runtime histograms/counters:
- `vulcan.daemon.request.duration_ms`
- `vulcan.provider.request.duration_ms`
- `vulcan.tool.call.duration_ms`
- `vulcan.hook.event.duration_ms`
- `vulcan.tokens.input`
- `vulcan.tokens.output`
- `vulcan.errors.total`
- `vulcan.tui.frame.draw_ms`
- `vulcan.tui.frame.interval_ms`
- `vulcan.tui.frames.total`
- `vulcan.tui.fps`
- `vulcan.tui.surface.count`
- `vulcan.process.memory.rss_bytes`
- `vulcan.process.cpu.percent`

**Step 2: Run test to verify it fails**

Run: `TMPDIR=/home/yycholla/vulcan-test-tmp cargo test observability::tests::stable_metric_names_cover_runtime_performance`

**Step 3: Implement minimal helper surface**

Add metric constants and small helper functions that emit tracing metric events with stable field names. Keep labels low-cardinality: `surface`, `operation`, `outcome`, `error_kind`, `rpc_method`, `provider`, `provider_mode`, `tool_name`, `hook_event`, `hook_handler`.

**Step 4: Run test to verify it passes**

Run: `TMPDIR=/home/yycholla/vulcan-test-tmp cargo test observability`

**Step 5: Commit**

Commit: `feat(observability): add runtime metric vocabulary`

### Task 2: Backend Runtime Metrics

**Files:**
- Modify: `src/daemon/server.rs`
- Modify: `src/agent/run.rs`
- Modify: `src/agent/dispatch.rs`
- Modify: `src/hooks/mod.rs`

**Step 1: Write focused tests**

Add/extend tests around stable outcome mapping where pure helpers exist. Prefer pure helper tests over trying to assert exporter output.

**Step 2: Instrument existing boundaries**

Record duration histograms and error/token counters at existing span boundaries:
- daemon request writeback
- provider request completion
- tool dispatch completion
- hook dispatch completion

**Step 3: Verify**

Run:
- `TMPDIR=/home/yycholla/vulcan-test-tmp cargo test observability`
- `TMPDIR=/home/yycholla/vulcan-test-tmp cargo test daemon::server::tests`
- `TMPDIR=/home/yycholla/vulcan-test-tmp cargo test run_record_captures_streaming_turn_with_tui_origin`
- `TMPDIR=/home/yycholla/vulcan-test-tmp cargo check -p vulcan`

**Step 4: Commit**

Commit: `feat(observability): record backend runtime metrics`

### Task 3: TUI Frame Performance Metrics

**Files:**
- Modify: `src/tui/mod.rs`
- Modify: `src/tui/state/mod.rs`
- Modify: `src/observability.rs`

**Step 1: Write focused tests**

Add a pure test for frame metric snapshot/FPS calculation if new state helpers are introduced.

**Step 2: Instrument the draw loop**

Record:
- draw duration per frame
- interval between frames
- frames total
- rolling FPS
- active frontend surface count

Do not create per-frame spans.

**Step 3: Verify**

Run:
- `TMPDIR=/home/yycholla/vulcan-test-tmp cargo test tui::`
- `TMPDIR=/home/yycholla/vulcan-test-tmp cargo check -p vulcan`
- `cargo fmt --check`

**Step 4: Commit**

Commit: `feat(observability): record tui frame metrics`

### Task 4: Process Resource Metrics

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/observability.rs`
- Modify: daemon/TUI startup path that initializes observability

**Step 1: Select dependency**

Use `sysinfo` only if it is not already available transitively in a usable way. Keep sampling periodic and cheap.

**Step 2: Add process sampler**

Emit:
- RSS memory bytes
- CPU percent
- thread count if available

Sample at the observability export interval or a bounded minimum such as 5 seconds.

**Step 3: Verify**

Run:
- `TMPDIR=/home/yycholla/vulcan-test-tmp cargo test observability`
- `TMPDIR=/home/yycholla/vulcan-test-tmp cargo check -p vulcan`
- `TMPDIR=/home/yycholla/vulcan-test-tmp cargo clippy --all-targets`

**Step 4: Commit**

Commit: `feat(observability): sample process resource metrics`
