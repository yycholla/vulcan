---
title: CONVENTIONS
last_mapped_commit: b48a9a7197a90fc5410b05ac5b66b4b2797dba6e
mapped_at: 2026-05-03
scope: full repo
---

# Coding Conventions

**Analysis Date:** 2026-05-03

## Naming Patterns

**Files:**
- Use Rust snake_case module files under `src/`, such as `src/provider/openai.rs`, `src/tools/fs_sandbox.rs`, `src/daemon/config_watch.rs`, and `src/tui/chat_render.rs`.
- Use `mod.rs` for module roots with several siblings, such as `src/hooks/mod.rs`, `src/tools/mod.rs`, `src/agent/mod.rs`, and `src/gateway/mod.rs`.
- Use `tests.rs` or `*_tests.rs` when tests are split out of a large module, such as `src/agent/tests.rs`, `src/config/tests.rs`, `src/daemon/protocol_tests.rs`, and `src/tui/state/tests.rs`.
- Use crate-prefixed extension directories for workspace members, such as `vulcan-ext-todo/src/lib.rs`, `vulcan-ext-auto-commit/src/lib.rs`, and `vulcan-frontend-api/src/lib.rs`.

**Functions:**
- Use snake_case for functions and methods: `validate_rewrite_history` in `src/hooks/mod.rs`, `parse_tool_params` in `src/tools/mod.rs`, `vulcan_home` in `src/config/mod.rs`, and `parse_request_strict` in `src/daemon/protocol.rs`.
- Constructor-style methods use `new`, `try_new`, or domain-specific builders: `OpenAIProvider::new` in `src/provider/openai.rs`, `SessionStore::try_new` in `src/memory/mod.rs`, and `Agent::builder` in `src/agent/mod.rs`.
- Boolean and classification helpers should read as predicates or selectors: `ProviderDebugMode::logs_wire` in `src/provider/openai.rs`, `ReplaySafety::as_str` in `src/tools/mod.rs`, and `DiffStyle::parse` in `src/tui/state/mod.rs`.

**Variables:**
- Use short local names only for tightly scoped parsed values (`p` in tool parameter handlers in `src/tools/file.rs`); use descriptive names for persisted or cross-boundary state (`captured_calls` in `src/provider/mock.rs`, `provider_label` in `src/tui/state/mod.rs`).
- Config and wire fields mirror serialized names in snake_case through `serde` defaults and tags, such as `provider_profile` in `src/memory/mod.rs`, `retryable` in `src/daemon/protocol.rs`, and `requires_user_approval` in `src/extensions/manifest.rs`.
- Constants use SCREAMING_SNAKE_CASE for caps and protocol values: `DEFAULT_MAX_OUTPUT_TOKENS` in `src/provider/openai.rs`, `MAX_FRAME_BYTES` in `src/daemon/protocol.rs`, and `READ_FILE_MAX_BYTES` in `src/tools/file.rs`.

**Types:**
- Use PascalCase for structs, enums, and traits: `OpenAIProvider` in `src/provider/openai.rs`, `HookOutcome` and `HookHandler` in `src/hooks/mod.rs`, `ToolResult` and `Tool` in `src/tools/mod.rs`, and `ProtocolError` in `src/daemon/protocol.rs`.
- Domain-specific error enums should end in `Error`, derive `thiserror::Error`, and expose stable variants: `ProviderError` in `src/provider/mod.rs`, `ClientError` in `src/client/errors.rs`, `FsSandboxError` in `src/tools/fs_sandbox.rs`, `SsrfError` in `src/tools/web_ssrf.rs`, and `ManifestError` in `src/extensions/manifest.rs`.
- Test-only harness wrappers use local names such as `ProviderHandle` in `src/agent/tests.rs`, `tests/agent_loop.rs`, and `tests/contracts.rs`.

## Code Style

**Formatting:**
- Use Rust edition 2024 from `Cargo.toml`.
- Use the pinned Rust toolchain in `rust-toolchain.toml` (`1.93.1`) with `rustfmt` and `clippy` components.
- Run `cargo fmt --all -- --check`; formatting is configured by `rustfmt.toml`.
- Keep line wrapping and import formatting to rustfmt defaults. Do not hand-align large blocks outside macro or table output.

**Linting:**
- Use Cargo lint configuration in `Cargo.toml`; `unsafe_code = "deny"` and `unused_must_use = "warn"` are active workspace rules.
- Use `clippy.toml` for the project MSRV (`1.93.1`).
- Run `cargo clippy --all-targets --all-features`. CI currently treats clippy as informational with `continue-on-error` in `.github/workflows/ci.yml`, but new code should be clippy-clean.
- Prefer local `#[allow(...)]` only when the manifest baseline intentionally leaves a lint enabled for new code, as shown by `#[allow(clippy::too_many_arguments)]` on `OpenAIProvider::new` in `src/provider/openai.rs`.

## Import Organization

**Order:**
1. Local crate imports first when they are the dominant context, as in `src/provider/openai.rs` and `src/tools/file.rs`.
2. External crate imports after local imports, grouped by crate: `anyhow`, `serde`, `serde_json`, `tokio`, and `tokio_util`.
3. Standard library imports last in many modules, such as `src/provider/openai.rs`; some modules place `std` first when that improves local readability, as in `tests/agent_loop.rs`.
4. Module declarations (`pub mod ...`) stay near the top of module roots, as in `src/lib.rs`, `src/hooks/mod.rs`, and `src/gateway/routes/mod.rs`.

**Path Aliases:**
- Use `crate::...` for root-crate internals, such as `crate::tools::ToolResult` in `src/hooks/mod.rs`.
- Use `super::...` inside sibling module tests and implementation splits, such as `src/agent/tests.rs` and `src/tui/state/tests.rs`.
- Use external crate paths directly for workspace crates, such as `vulcan_frontend_api` in `src/tui/state/tests.rs` and `vulcan_ext_todo` in `vulcan-ext-todo/tests/todo_e2e.rs`.
- No custom Rust path aliasing is configured beyond crate/module names in `Cargo.toml`.

## Error Handling

**Patterns:**
- Use `anyhow::Result` for application, CLI, tool, and async runtime paths where callers need contextual failures: examples include `src/config/mod.rs`, `src/tools/file.rs`, `src/agent/run.rs`, and `src/gateway/mod.rs`.
- Add context at I/O, parsing, and persistence boundaries with `.context(...)` or `.with_context(...)`, as in `atomic_write` and `snapshot_bak` in `src/config/mod.rs`.
- Use `anyhow::bail!` for early user-facing command failures in CLI/tool code, such as `src/cli_provider.rs`, `src/cli_gateway.rs`, `src/tools/shell.rs`, and `src/tools/mod.rs`.
- Use typed errors when callers branch on error kind or serialize a protocol error: `ProviderError` in `src/provider/mod.rs`, `ProtocolError` in `src/daemon/protocol.rs`, `ClientError` in `src/client/errors.rs`, `FsSandboxError` in `src/tools/fs_sandbox.rs`, and `ManifestError` in `src/extensions/manifest.rs`.
- Tool implementations should return `Ok(ToolResult::err(...))` for model-visible validation or execution failures and reserve `Err(anyhow::Error)` for infrastructure failures. `src/tools/file.rs` and `src/tools/mod.rs` show this split.
- Provider and wire logging must redact secrets through `src/provider/redact.rs`; `src/provider/openai.rs` calls `redact_value` and `redact_response_text` before wire-debug logging.

## Logging

**Framework:** `tracing`

**Patterns:**
- Use structured `tracing::info!`, `tracing::warn!`, `tracing::debug!`, and `tracing::error!` for runtime behavior, as in `src/agent/run.rs`, `src/hooks/mod.rs`, `src/runtime_pool.rs`, and `src/tools/shell.rs`.
- Use spans for cross-cutting observability: `tool_call_span`, `daemon_request_span`, and provider spans live in `src/observability.rs`; hook execution spans are built in `src/hooks/mod.rs`.
- TUI runtime code should log through tracing instead of printing to stdout; repo docs note TUI logs go to a file, and `docs/testing/overview.md` plus `context_pack` guidance call out avoiding `println!` in TUI mode.
- CLI subcommands may use `println!` for intentional user output, as in `src/cli_provider.rs`, `src/cli_extension.rs`, and `src/cli_config.rs`.
- Use `eprintln!` sparingly for process-level fallback or stub binaries, such as telemetry shutdown failures in `src/observability.rs` and the placeholder `vulcan-tui/src/main.rs`.

## Comments

**When to Comment:**
- Comment invariants and contracts that future edits must preserve: hook semantics in `src/hooks/mod.rs`, daemon protocol framing in `src/daemon/protocol.rs`, TUI threading in `src/tui/state/mod.rs`, and tool replay safety in `src/tools/mod.rs`.
- Comment security, resource, or data-loss constraints close to the guard: redaction in `src/provider/openai.rs`, file-size caps in `src/tools/file.rs`, config atomic writes in `src/config/mod.rs`, and socket permissions in `tests/daemon_e2e.rs`.
- Keep issue-key comments only when they explain why behavior exists or pins acceptance criteria. Examples include YYC references in `src/tools/file.rs`, `src/agent/tests.rs`, and `tests/contracts.rs`.

**JSDoc/TSDoc:**
- Not applicable; this is a Rust workspace.
- Use Rust doc comments (`///` and `//!`) for public APIs and module-level contracts, as shown in `src/daemon/protocol.rs`, `src/tools/mod.rs`, `src/provider/mock.rs`, and `src/extensions/manifest.rs`.

## Function Design

**Size:** Keep small pure helpers near the code they support (`default_read_offset`, `default_list_path`, and `valid_id`), but allow larger orchestrators where they encode a state machine or protocol boundary (`Agent` run paths in `src/agent/run.rs`, hook dispatch in `src/hooks/mod.rs`, TUI state in `src/tui/state/mod.rs`).

**Parameters:** Prefer typed parameter structs for JSON/tool inputs. New tools should use `#[derive(Deserialize)]` params plus `parse_tool_params` from `src/tools/mod.rs`, following `ReadFileParams` and `ListFilesParams` in `src/tools/file.rs`.

**Return Values:** Use `anyhow::Result<T>` at infrastructure boundaries, typed `std::result::Result<T, DomainError>` where the caller needs variants, and `ToolResult` for LLM-facing tool output. Use builder-style `with_*` methods for optional result metadata, as in `ToolResult::with_details`, `ToolResult::with_display_preview`, and `ToolResult::with_edit_diff` in `src/tools/mod.rs`.

## Module Design

**Exports:** Root module exports are explicit in `src/lib.rs`. Feature-gated modules use `#[cfg(feature = "...")]`, such as `client`, `daemon`, `gateway`, and `cli_gateway`.

**Barrel Files:** Use module roots as controlled barrels only when they define shared contracts and re-export compatibility types. `src/tools/mod.rs` owns the `Tool` contract and registry; `src/hooks/mod.rs` owns hook contracts; `src/tui/state/mod.rs` re-exports legacy state imports for sibling modules.

**Project-Specific Constraints:**
- Keep hook behavior centralized in `src/hooks/mod.rs`; built-in hooks live under `src/hooks/` and are registered through the long-lived agent path in `src/agent/mod.rs`.
- Keep tool implementations under `src/tools/`, register them through `ToolRegistry` in `src/tools/mod.rs`, and classify replay safety with `ReplaySafety`.
- Keep provider-compatible behavior in `src/provider/`; buffered and streaming paths should stay aligned through `LLMProvider` in `src/provider/mod.rs` and `OpenAIProvider` in `src/provider/openai.rs`.
- Keep daemon/client wire contracts in `src/daemon/protocol.rs` and client transport errors in `src/client/errors.rs`.
- Keep extension crate behavior in `vulcan-ext-*` members and shared frontend contracts in `vulcan-frontend-api/src/lib.rs`.

---

*Convention analysis: 2026-05-03*
