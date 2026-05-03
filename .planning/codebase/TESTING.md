---
title: TESTING
last_mapped_commit: b48a9a7197a90fc5410b05ac5b66b4b2797dba6e
mapped_at: 2026-05-03
scope: full repo
---

# Testing Patterns

**Analysis Date:** 2026-05-03

## Test Framework

**Runner:**
- Rust built-in test harness via `cargo test`, with async tests through `tokio::test`.
- CI uses `cargo-nextest` with config in `.config/nextest.toml`.
- Root dev dependencies live in `Cargo.toml`: `tempfile`, `tokio` with `test-util`, `insta`, `assert_cmd`, `predicates`, `divan`, and `hdrhistogram`.
- Benchmarks use `divan` targets in `benches/tui_render.rs` and `benches/agent_core.rs`; soak testing uses the `vulcan-soak` bin at `benches/soak.rs`.

**Assertion Library:**
- Standard Rust assertions: `assert_eq!`, `assert!`, `matches!`, and explicit `panic!` messages.
- Snapshot assertions through `insta`, used in `src/prompt_builder.rs`, `src/tui/chat_render.rs`, and `src/tui/rendering.rs`.
- CLI process assertions through `assert_cmd` and `predicates`, used in `tests/daemon_e2e.rs` and supported by `tests/support/mod.rs`.

**Run Commands:**
```bash
cargo test                                      # Run all default-feature tests
cargo test --features gateway gateway::        # Run gateway-focused feature tests
cargo test <name_substring>                    # Run tests matching a name substring
cargo test --doc                               # Run doc tests with default features
cargo test --doc --features gateway            # Run doc tests with gateway enabled
cargo nextest run --profile ci --lib --bins --tests
cargo nextest run --profile ci --lib --bins --tests --features gateway
cargo llvm-cov nextest --all-features --workspace --lcov --output-path lcov.info
cargo bench                                    # Run divan benchmarks
cargo run --release --bin vulcan-soak --features bench-soak -- --turns 20
```

## Test File Organization

**Location:**
- Unit tests are usually colocated in `#[cfg(test)] mod tests` inside implementation modules, such as `src/hooks/mod.rs`, `src/tools/file.rs`, `src/gateway/queue.rs`, `src/extensions/manifest.rs`, and `vulcan-ext-todo/src/lib.rs`.
- Large module test suites can be split into sibling files named `tests.rs` or `*_tests.rs`, such as `src/agent/tests.rs`, `src/memory/tests.rs`, `src/config/tests.rs`, `src/daemon/lifecycle_tests.rs`, `src/daemon/protocol_tests.rs`, and `src/tui/state/tests.rs`.
- Integration tests live under root `tests/`: `tests/agent_loop.rs`, `tests/contracts.rs`, `tests/client_autostart.rs`, `tests/daemon_e2e.rs`, `tests/frontend_extensions.rs`, and `tests/gateway_no_agent_map.rs`.
- Extension crate integration tests live in the extension crate, such as `vulcan-ext-todo/tests/todo_e2e.rs`.
- Snapshot files live under `src/snapshots/`, for example `src/snapshots/vulcan__prompt_builder__tests__system_prompt_default_registry.snap`.

**Naming:**
- Use behavior-focused snake_case test names: `sanitize_drops_orphan_tool_with_no_preceding_assistant` in `src/agent/tests.rs`, `daemon_socket_is_0600` in `tests/daemon_e2e.rs`, and `readonly_profile_does_not_expose_mutating_tools` in `tests/contracts.rs`.
- Use comments above contract tests when the test pins a design invariant or acceptance criterion, as in `tests/contracts.rs` and `tests/agent_loop.rs`.
- Use feature gates at file/module level for feature-specific tests, such as `#![cfg(feature = "daemon")]` in `tests/daemon_e2e.rs`.

**Structure:**
```text
src/<module>.rs                 # inline #[cfg(test)] mod tests for local pure behavior
src/<module>/tests.rs           # split module test suite for larger modules
tests/<behavior>.rs             # root integration tests using public crate APIs
tests/support/mod.rs            # shared integration helpers
vulcan-ext-*/tests/*.rs         # extension-crate E2E/integration coverage
src/snapshots/*.snap            # insta snapshot baselines
benches/*.rs                    # performance and soak measurement targets
```

## Test Structure

**Suite Organization:**
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_id_characters() {
        let raw = r#"id = "Lint Helper!""#;
        let err = ExtensionManifest::from_toml_str(raw).unwrap_err();
        match err {
            ManifestError::InvalidField { field, .. } => assert_eq!(field, "id"),
            other => panic!("expected InvalidField, got {other:?}"),
        }
    }
}
```

**Patterns:**
- Build small fixture helpers near tests: `asst_with_tool_calls`, `tool_msg`, and `agent_with_mock` in `src/agent/tests.rs`; `vulcan_with_home` and `wait_for_socket` in `tests/daemon_e2e.rs`.
- Use Arrange/Act/Assert in one function without explicit section comments unless the behavior is complex. `tests/agent_loop.rs` uses comments to pin lifecycle contracts.
- Use `#[tokio::test]` for async agent, hook, gateway, tool, and extension tests; use `#[test]` for pure parsing, formatting, rendering, and state transitions.
- Assert exact outputs for stable contracts (`assert_eq!`) and use substring/predicate checks for process output or generated text, as in `tests/daemon_e2e.rs`.

## Mocking

**Framework:** Hand-rolled fakes and in-memory stores; no external mocking crate detected.

**Patterns:**
```rust
let mock = Arc::new(MockProvider::new(128_000));
mock.enqueue_tool_call(
    "read_file",
    "read_missing",
    serde_json::json!({"path": "/this/does/not/exist/yyc-193"}),
);
mock.enqueue_text("could not read.");
let agent = Agent::for_test(
    Box::new(ProviderHandle(mock.clone())),
    ToolRegistry::new(),
    HookRegistry::new(),
    Arc::new(SkillRegistry::empty()),
);
```

**What to Mock:**
- Mock LLM/provider behavior with `src/provider/mock.rs`; tests enqueue text, tool calls, mixed responses, reasoning, or errors and inspect `captured_calls`.
- Use in-memory stores for persistence contracts where possible, such as `SessionStore::in_memory()` in `vulcan-ext-todo/tests/todo_e2e.rs`.
- Use `tempfile::tempdir` for filesystem, daemon home, config, and missing-path tests, as in `tests/daemon_e2e.rs`, `tests/agent_loop.rs`, and `src/tools/file.rs`.
- Use public crate APIs and small wrapper structs (`ProviderHandle`) instead of patching private internals, as shown in `tests/contracts.rs`.

**What NOT to Mock:**
- Do not use live provider/network calls in normal tests; provider behavior should go through `MockProvider` or local HTTP/router harnesses.
- Do not depend on a developer's real `~/.vulcan`; redirect `VULCAN_HOME` to a temp directory for daemon/config tests, as in `tests/daemon_e2e.rs`.
- Do not mock tool registry filtering when the test is about profiles or tool exposure; build a real `ToolRegistry` and apply the profile as in `tests/contracts.rs`.
- Do not rerun mutating tools in replay tests unless their `ReplaySafety` explicitly permits it; tool replay safety is defined in `src/tools/mod.rs`.

## Fixtures and Factories

**Test Data:**
```rust
fn empty_skills() -> Arc<SkillRegistry> {
    Arc::new(SkillRegistry::empty())
}

fn agent_with_profile(profile: Option<ToolProfile>) -> (Agent, Arc<MockProvider>) {
    let mock = Arc::new(MockProvider::new(128_000));
    let mut tools = ToolRegistry::new();
    if let Some(p) = profile {
        tools.apply_profile(&p);
    }
    let agent = Agent::for_test(
        Box::new(ProviderHandle(mock.clone())),
        tools,
        HookRegistry::new(),
        empty_skills(),
    );
    (agent, mock)
}
```

**Location:**
- Shared integration helpers live in `tests/support/mod.rs`, especially `vulcan_command()` and binary-build fallback logic for process tests.
- Agent mock and generated benchmark provider live in `src/provider/mock.rs`.
- Test-local factories stay inside their suite when they are specific to one behavior, such as `agent_with_mock` in `src/agent/tests.rs` and `agent_with_profile` in `tests/contracts.rs`.
- Snapshot baselines live in `src/snapshots/`; inline snapshots are also used in `src/tui/rendering.rs`.

## Coverage

**Requirements:** No hard coverage threshold is configured.

**View Coverage:**
```bash
cargo llvm-cov nextest --all-features --workspace --lcov --output-path lcov.info
cargo llvm-cov report --summary-only
```

- Coverage runs only on pushes in `.github/workflows/ci.yml`, not as a PR gate.
- CI uploads `lcov.info` as `coverage-lcov` with 14-day retention.
- The repository currently has broad unit/integration coverage: root exploration found 949 `#[test]` entries and 362 `#[tokio::test]` entries under `src`, `tests`, and `vulcan-*` Rust files.

## Test Types

**Unit Tests:**
- Scope pure parsing, formatting, validation, state transitions, and small helpers in the owning module.
- Examples: `src/extensions/manifest.rs` validates manifest parsing; `src/tools/fs_sandbox.rs` validates sandbox decisions; `src/tui/state/tests.rs` validates UI state transitions; `src/provider/openai.rs` contains provider parsing/retry helper tests.

**Integration Tests:**
- Scope public agent, daemon, client, gateway, contract, and extension behavior.
- Examples: `tests/agent_loop.rs` covers agent lifecycle/run records; `tests/contracts.rs` pins high-level tool profile contracts; `tests/daemon_e2e.rs` exercises the real `vulcan` binary; `tests/frontend_extensions.rs` covers extension/frontend contracts; `vulcan-ext-todo/tests/todo_e2e.rs` covers extension replay across session end/start.

**E2E Tests:**
- CLI/daemon E2E uses `assert_cmd` and a real built `vulcan` binary through `tests/support/mod.rs`.
- Extension E2E uses real registries, hooks, tools, and in-memory session storage in `vulcan-ext-todo/tests/todo_e2e.rs`.
- No browser/UI E2E framework detected.

**Benchmark Tests:**
- Divan benchmark targets are declared in `Cargo.toml` and implemented in `benches/tui_render.rs` and `benches/agent_core.rs`.
- The soak binary is declared as `[[bin]] vulcan-soak` in `Cargo.toml` and implemented in `benches/soak.rs`.
- Benchmark CI runs median-of-3 measurement and informational baseline diffing in `.github/workflows/bench.yml`.

## Common Patterns

**Async Testing:**
```rust
#[tokio::test]
async fn todo_details_survive_session_end_then_session_start_replay() {
    let memory = Arc::new(SessionStore::in_memory());
    let hooks = HookRegistry::new();
    let mut tools = ToolRegistry::new();
    // Wire real registry/hooks/tools, execute, then assert persisted replay.
}
```

**Error Testing:**
```rust
let err = ExtensionManifest::from_toml_str(raw).unwrap_err();
match err {
    ManifestError::InvalidField { field, .. } => assert_eq!(field, "id"),
    other => panic!("expected InvalidField, got {other:?}"),
}
```

**Process Testing:**
```rust
vulcan_with_home(dir.path())
    .args(["daemon", "status"])
    .assert()
    .success()
    .stdout(predicate::str::contains("pid"))
    .stdout(predicate::str::contains("uptime_secs"));
```

**Snapshot Testing:**
```rust
insta::assert_snapshot!("system_prompt_default_registry", normalized);
```

## CI Quality Gates

- Formatting: `.github/workflows/ci.yml` runs `cargo fmt --all -- --check`.
- Linting: `.github/workflows/ci.yml` runs `cargo clippy --all-targets --all-features`; it is currently `continue-on-error` because of an existing baseline.
- Build coverage: CI builds default features and `--features gateway` with `cargo build --all-targets`.
- Test execution: CI runs nextest for default and gateway feature sets and uploads `target/nextest/ci/junit.xml`.
- Doc tests: CI runs `cargo test --doc` and `cargo test --doc --features gateway`.
- Feature coverage: push workflow runs `cargo hack check --feature-powerset --no-dev-deps --exclude-features bench-soak`.
- Supply-chain/dependency checks: CI runs `cargo deny check` using `deny.toml` and cargo-machete for unused dependencies.

---

*Testing analysis: 2026-05-03*
