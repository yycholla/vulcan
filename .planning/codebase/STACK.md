---
title: STACK
last_mapped_commit: f5e07ca31dd7a4ac794b9838884bfdad74a7e37c
mapped_at: 2026-05-03
scope: full repo
---

# Stack

Vulcan is a Rust workspace for a personal AI agent with CLI, TUI, daemon, gateway, extension, and orchestration surfaces.

## Languages And Runtime

- Primary language: Rust, edition 2024, declared in `Cargo.toml`.
- Async runtime: Tokio, used across the daemon, provider calls, gateway workers, TUI command paths, and tests.
- Main binary package: `vulcan`, with entry point in `vulcan/src/main.rs`.
- Core library package: root `Cargo.toml` publishes crate name `vulcan` from `src/lib.rs`.
- Terminal UI runtime is in `src/tui/` with support crates under `vulcan-tui/` and `vulcan-frontend-api/`.

## Workspace Crates

- Root library crate: `src/lib.rs`, plus most production modules under `src/`.
- Binary wrapper crate: `vulcan/`, linking first-party extensions through `extern crate ... as _`.
- Frontend API crate: `vulcan-frontend-api/`, used by extension and TUI boundaries.
- TUI crate: `vulcan-tui/`, supporting frontend rendering concerns.
- Extension macro crate: `vulcan-extension-macros/`.
- Built-in extension crates: `vulcan-core-ext-skills/` and `vulcan-core-ext-safety/`.
- Demo/product extension crates: `vulcan-ext-auto-commit/`, `vulcan-ext-compact-summary/`, `vulcan-ext-input-demo/`, `vulcan-ext-snake/`, `vulcan-ext-spinner-demo/`, and `vulcan-ext-todo/`.

## Feature Flags

- Default feature: `daemon`, making daemon-client integration part of the normal build.
- `gateway` enables Axum HTTP routes, gateway queues, Discord, auth helpers, and daemon integration.
- `telegram` depends on `gateway` and enables Telegram connector support.
- Optional gateway dependencies include `axum`, `tower`, `subtle`, `hmac`, `serenity`, `r2d2`, `r2d2_sqlite`, and `teloxide-core`.

## Core Dependencies

- CLI/config: `clap`, `clap_complete`, `dialoguer`, `toml`, `toml_edit`, `serde`, `serde_json`, and `serde_yaml`.
- TUI: `ratatui`, `termwiz`, `tui-textarea-2`, `figlet-rs`, and `owo-colors`.
- Providers and web: `reqwest`, `futures-util`, `url`, `scraper`, and `html2text`.
- Observability: `tracing`, `tracing-subscriber`, `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, and `tracing-opentelemetry`.
- Storage and local state: `rusqlite` with bundled SQLite, `redb`, `uuid`, and `chrono`.
- Code intelligence: `tree-sitter`, language grammars, `lsp-types`, `ignore`, `globset`, `gix`, and `portable-pty`.
- Memory and embeddings: `cortex-memory-core` as `cortex_core`, `fastembed`, and `tiktoken-rs`.

## Configuration

- Example config lives in `config.example.toml`.
- Runtime config path is `~/.vulcan/config.toml`.
- Provider API key is read from config or `VULCAN_API_KEY`.
- Provider defaults in the example use an OpenAI-compatible OpenRouter endpoint.
- Observability is configured under `[observability]` with OTLP HTTP endpoint, traces, metrics, export interval, service name, and surface toggles.
- Gateway config lives under `[gateway]`, including bind address, auth token, platform connectors, commands, and queue settings.

## User-Facing Surfaces

- `cargo run` launches the TUI by default through `vulcan/src/main.rs`.
- `cargo run -- prompt "text"` runs one-shot prompt mode.
- `cargo run -- session <id>` resumes saved sessions.
- `cargo run --features gateway -- gateway` runs gateway daemon mode when gateway support is enabled.
- Slash commands and model/provider selection are implemented through the TUI and CLI modules in `src/tui/` and `src/cli_*.rs`.

## Build And Tooling

- Standard build: `cargo build --all-targets`.
- Standard tests: `cargo test`.
- Gateway tests: `cargo test --features gateway gateway::`.
- Release build: `cargo build --release`.
- Formatting is controlled by `rustfmt.toml`.
- Lints and dependency checks are configured by `clippy.toml` and `deny.toml`.
