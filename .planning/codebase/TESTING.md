---
title: TESTING
last_mapped_commit: f5e07ca31dd7a4ac794b9838884bfdad74a7e37c
mapped_at: 2026-05-03
scope: full repo
---

# Testing

Vulcan uses cargo tests, nextest in CI, module-level unit tests, integration tests, feature builds, dependency checks, and benchmark workflows.

## Local Commands

- Standard test command: `cargo test`.
- Full compile command from repo guidance: `cargo build --all-targets`.
- Gateway-focused tests: `cargo test --features gateway gateway::`.
- Single test filtering uses `cargo test <name_substring>`.
- Release build command: `cargo build --release`.
- TUI can be launched with `cargo run`.
- One-shot mode can be tested with `cargo run -- prompt "your text"`.

## Integration Tests

- Daemon end-to-end coverage lives in `tests/daemon_e2e.rs`.
- Client auto-start behavior lives in `tests/client_autostart.rs`.
- Agent loop contracts live in `tests/agent_loop.rs`.
- Frontend extension contracts live in `tests/frontend_extensions.rs`.
- Gateway behavior without agent map coverage lives in `tests/gateway_no_agent_map.rs`.
- Shared integration helpers live in `tests/support/mod.rs`.
- Contract-level tests live in `tests/contracts.rs`.

## Module Tests

- Agent tests live in `src/agent/tests.rs`.
- Config tests live in `src/config/tests.rs`.
- Daemon lifecycle tests live in `src/daemon/lifecycle_tests.rs`.
- Daemon protocol tests live in `src/daemon/protocol_tests.rs`.
- Memory tests live in `src/memory/tests.rs`.
- TUI state tests live under `src/tui/state/tests.rs` when present in the module tree.
- Additional inline tests are colocated with implementation modules.

## Extension Tests

- Todo extension E2E tests live in `vulcan-ext-todo/tests/todo_e2e.rs`.
- Frontend extension integration is also tested from the root in `tests/frontend_extensions.rs`.
- First-party extension crates should be tested both as independent crates and through root integration where daemon/frontend contracts matter.

## CI Workflow

- CI configuration lives in `.github/workflows/ci.yml`.
- Pull requests and main pushes run formatting, clippy, tests, and dependency checks.
- CI sets `RUSTFLAGS=-D warnings`.
- Formatting runs `cargo fmt --all -- --check`.
- Clippy runs `cargo clippy --all-targets --all-features`.
- Clippy is currently `continue-on-error` because the repository has an existing warning baseline.
- Test jobs build default and gateway feature sets, build the CLI, run nextest, run doc tests, and upload JUnit output.

## Benchmarks And Performance

- Benchmark workflow lives in `.github/workflows/bench.yml`.
- Benchmarks run on relevant PR paths, schedule, and manual dispatch.
- The workflow runs `cargo bench`, `vulcan-soak`, median-of-3 measurement, and informational baseline comparison.
- TUI render benchmark entry point is `src/bin/tui-render-bench.rs`.
- Observability metrics are being added to help diagnose daemon, provider, hook, tool, TUI, and process performance.

## Quality Gates

- Dependency policy is checked through `cargo deny` using `deny.toml`.
- Unused dependency detection is handled by cargo-machete in CI.
- Feature coverage includes default and gateway builds in normal CI and broader powerset coverage on push workflows.
- Coverage reporting runs on push workflows.
- Doc tests run for default and gateway feature sets.

## Test Environment Notes

- Some tests need stable working directories or temp directories because tools execute shell/file operations.
- Daemon and TUI tests should avoid constructing fresh agents per prompt when long-lived state is the behavior under test.
- Gateway tests should consider durable queues and platform lane mapping.
- Provider tests should avoid live network assumptions unless explicitly marked or mocked through `src/provider/mock.rs`.
