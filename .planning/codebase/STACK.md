---
title: STACK
last_mapped_commit: b48a9a7197a90fc5410b05ac5b66b4b2797dba6e
mapped_at: 2026-05-03
scope: full repo
---

# Technology Stack

**Analysis Date:** 2026-05-03

Vulcan is a Rust workspace for a local AI agent with CLI, TUI, daemon, optional HTTP gateway, local persistence, code-intelligence tools, and first-party extension crates.

## Languages

**Primary:**
- Rust 1.93.1, edition 2024 - workspace language for all production crates; pinned in `rust-toolchain.toml` and declared across `Cargo.toml`, `vulcan/Cargo.toml`, `vulcan-tui/Cargo.toml`, `vulcan-frontend-api/Cargo.toml`, and extension crate manifests.

**Secondary:**
- Python 3 - CI/benchmark helper scripts in `scripts/bench-diff.py` and `scripts/median-of-3.py`.
- TOML - runtime configuration and manifests in `config.example.toml`, `Cargo.toml`, `rustfmt.toml`, `clippy.toml`, and `deny.toml`.
- YAML - GitHub Actions workflows in `.github/workflows/ci.yml` and `.github/workflows/bench.yml`.
- Markdown - user and agent documentation in `README.md`, `docs/`, `CONTEXT.md`, and `CONTEXT-MAP.md`.

## Runtime

**Environment:**
- Rust toolchain `1.93.1` with `rustfmt` and `clippy`, configured by `rust-toolchain.toml`.
- Tokio async runtime `1` with `full` features, used by the agent, daemon, gateway, provider calls, shell processes, and async tests through `Cargo.toml`.
- Unix-style daemon/frontend IPC is enabled by the default `daemon` feature and implemented under `src/daemon/` and `src/client/`.
- Gateway mode is optional behind the `gateway` feature and runs an Axum server from `src/gateway/server.rs`.

**Package Manager:**
- Cargo workspace, resolver 2, declared in root `Cargo.toml`.
- Lockfile: present at `Cargo.lock`.
- Main binary package: `vulcan/Cargo.toml`.
- Core library package: root `Cargo.toml` publishes package `vulcan-core` with library crate name `vulcan` from `src/lib.rs`.

## Frameworks

**Core:**
- Tokio `1` - async runtime, process management, timers, tasks, and channels across `src/agent/`, `src/daemon/`, `src/gateway/`, and `src/tools/`.
- Clap `4` plus `clap_complete` `4.6` - CLI command tree and shell completions in `src/cli.rs` and `vulcan/src/main.rs`.
- Ratatui `0.30` plus Termwiz `0.23.3` and `tui-textarea-2` `0.10.2` - terminal UI rendering and input under `src/tui/` and `vulcan-tui/`.
- Axum `0.8.9` plus Tower `0.5.3` - optional gateway HTTP routes under `src/gateway/server.rs` and `src/gateway/routes/`.
- Inventory `0.3.24` - compile-time extension registration through `src/extensions/api.rs`, `vulcan-frontend-api/src/lib.rs`, and first-party extension crates.

**Testing:**
- Built-in Rust test harness - unit and integration tests under `src/**`, `tests/`, and extension crate `tests/` directories.
- Tokio test utilities - async tests via `tokio` dev dependency in `Cargo.toml`.
- Insta `1.47.2` - snapshot testing for prompt/TUI surfaces from `Cargo.toml`.
- Assert Cmd `2.2.1` and Predicates `3.1.4` - binary/E2E tests in `tests/`.
- Divan `0.1` and HDRHistogram `7` - benchmark harnesses in `benches/agent_core.rs`, `benches/tui_render.rs`, and `benches/soak.rs`.

**Build/Dev:**
- Rustfmt - formatting configured by `rustfmt.toml`.
- Clippy - lint baseline configured in root `Cargo.toml` and `clippy.toml`.
- Cargo Deny - dependency policy in `deny.toml` and CI job in `.github/workflows/ci.yml`.
- Cargo Nextest - CI test runner in `.github/workflows/ci.yml`.
- Cargo LLVM Cov - coverage on pushes to `main` in `.github/workflows/ci.yml`.
- Cargo Hack - feature powerset check on `main` in `.github/workflows/ci.yml`.
- Cargo Machete - unused dependency check in `.github/workflows/ci.yml`.

## Key Dependencies

**Critical:**
- `reqwest` `0.13` - OpenAI-compatible provider calls in `src/provider/openai.rs`, provider catalogs in `src/provider/catalog.rs`, model fetch in `src/cli_model.rs`, embeddings in `src/code/embed.rs`, and web tools in `src/tools/web.rs`.
- `serde`, `serde_json`, `serde_yaml`, `toml`, `toml_edit` - config, protocol, extension manifest, queue payload, and provider JSON handling across `src/config/`, `src/daemon/protocol.rs`, `src/extensions/`, and `src/gateway/`.
- `rusqlite` `0.39.0` with bundled SQLite - session store, gateway queues, artifact store, run records, playbooks, code graph, and embeddings in `src/memory/`, `src/gateway/queue.rs`, `src/artifact/mod.rs`, `src/run_record/mod.rs`, `src/playbook/mod.rs`, `src/code/graph.rs`, and `src/code/embed.rs`.
- `cortex-memory-core` `0.3.1` as `cortex_core` - optional graph memory under `src/memory/cortex.rs` and `src/cli_cortex.rs`.
- `fastembed` `4.9.1` - Cortex embedding model mapping/config support in `src/memory/cortex.rs` and `src/cli_config.rs`.
- `tree-sitter` `0.26.8` plus Rust, Python, TypeScript, JavaScript, Go, and JSON grammars - code parsing in `src/code/`, `src/tools/code.rs`, `src/tools/code_edit.rs`, and `src/impact/generator.rs`.
- `lsp-types` `0.97` - LSP JSON-RPC request/response typing in `src/code/lsp/` and `src/tools/lsp.rs`.
- `portable-pty` `0.9.0` - shell/PTY execution in `src/tools/shell.rs`.
- `gix` `0.83` - repository discovery in `src/tools/git.rs`.

**Infrastructure:**
- `tracing`, `tracing-subscriber`, `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `tracing-opentelemetry` - structured logs, traces, and metrics in `src/observability.rs` and `vulcan/src/main.rs`.
- `r2d2` `0.8.10` and `r2d2_sqlite` `0.33.0` - optional gateway SQLite pooling in `src/memory/schema.rs` and `src/gateway/queue.rs`.
- `serenity` `0.12` - optional Discord gateway connector in `src/gateway/discord.rs`.
- `teloxide-core` `0.13.0` - optional Telegram connector in `src/gateway/telegram.rs`.
- `hmac`, `sha2`, `subtle`, `http` - webhook/bearer auth and signature verification in `src/gateway/server.rs`, `src/gateway/routes/webhook.rs`, `src/gateway/loopback.rs`, and `src/gateway/telegram.rs`.
- `cron` `0.16` and `chrono-tz` `0.10` - scheduled gateway jobs in `src/gateway/scheduler.rs` and `src/config/mod.rs`.
- `ignore`, `globset`, `percent-encoding`, `scraper`, `html2text` - workspace walking, glob filters, web search/fetch, and HTML parsing in `src/code/embed.rs`, `src/tools/web.rs`, and `src/tools/web_ssrf.rs`.
- `secrecy` `0.10` and provider redaction helpers - in-memory secret handling and log redaction in `src/provider/redact.rs`.

## Configuration

**Environment:**
- Primary runtime config path is `~/.vulcan/config.toml`; sample config lives at `config.example.toml`.
- `VULCAN_HOME` overrides the config/data directory in `src/config/mod.rs`.
- `VULCAN_API_KEY` is the global provider API-key fallback in `src/config/mod.rs`, `src/agent/provider.rs`, and `docs/configuration/overview.md`.
- Gateway shell commands receive `VULCAN_PLATFORM`, `VULCAN_CHAT_ID`, and `VULCAN_USER_ID` from `src/gateway/commands.rs`.
- Logging uses `VULCAN_LOG` in `vulcan/src/main.rs`.
- Extension test behavior uses `VULCAN_EXT_COMPACT_SUMMARY_MODE` in `vulcan-ext-compact-summary/src/lib.rs`.
- Do not read local `config.toml` for mapping; it may contain provider or gateway secrets.

**Build:**
- Workspace and feature flags: `Cargo.toml`.
- Binary wrapper: `vulcan/Cargo.toml`.
- Frontend extension API: `vulcan-frontend-api/Cargo.toml`.
- TUI support crate: `vulcan-tui/Cargo.toml`.
- Extension macros: `vulcan-extension-macros/Cargo.toml`.
- First-party extensions: `vulcan-core-ext-skills/Cargo.toml`, `vulcan-core-ext-safety/Cargo.toml`, `vulcan-ext-auto-commit/Cargo.toml`, `vulcan-ext-compact-summary/Cargo.toml`, `vulcan-ext-input-demo/Cargo.toml`, `vulcan-ext-snake/Cargo.toml`, `vulcan-ext-spinner-demo/Cargo.toml`, and `vulcan-ext-todo/Cargo.toml`.
- Release profile is size-oriented in root `Cargo.toml`: `opt-level = "z"`, LTO, one codegen unit, and stripped binary.
- Test profile trims debuginfo in root `Cargo.toml`.
- CI workflows are `.github/workflows/ci.yml` and `.github/workflows/bench.yml`.

## Platform Requirements

**Development:**
- Install the Rust toolchain from `rust-toolchain.toml`.
- Use `cargo build --all-targets` for full compile coverage.
- Use `cargo test` for the default test suite.
- Use `cargo test --features gateway gateway::` or CI's `cargo nextest run --features gateway` path for gateway coverage.
- Optional CI/local tools include `cargo-nextest`, `cargo-llvm-cov`, `cargo-hack`, `cargo-deny`, and `cargo-machete` per `.github/workflows/ci.yml`.

**Production:**
- Deployment target is a local Rust binary, not a hosted service, with data under `~/.vulcan/`.
- `cargo run` launches the default chat UI through `vulcan/src/main.rs`.
- `cargo run -- prompt "text"` runs one-shot mode.
- `cargo run -- session <id>` resumes a saved session.
- `cargo run --features gateway -- gateway run` runs the optional gateway daemon.
- `cargo build --release` creates the size-optimized binary configured in `Cargo.toml`.

## Project Skills And Constraints

- Project-local skill directory exists at `.agents/skills/find-skills/SKILL.md`; it describes discovery/install guidance for external agent skills and does not define Vulcan runtime architecture.
- Repo instructions in `CLAUDE.md` emphasize GitHub Issues/Graphite workflow, long-lived `Agent` and hook invariants, OpenAI-compatible provider behavior, and gateway daemon foundations.
- The project skill ecosystem also includes runtime skill support under `src/hooks/skills.rs`, `src/skills/mod.rs`, and the core skills extension crate `vulcan-core-ext-skills/`.

---

*Stack analysis: 2026-05-03*
