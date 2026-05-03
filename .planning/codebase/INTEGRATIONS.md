---
title: INTEGRATIONS
last_mapped_commit: f5e07ca31dd7a4ac794b9838884bfdad74a7e37c
mapped_at: 2026-05-03
scope: full repo
---

# Integrations

Vulcan integrates with remote model APIs, local operating-system resources, gateway platforms, OpenTelemetry collectors, and first-party extension crates.

## LLM Providers

- Provider abstraction lives under `src/provider/`.
- The OpenAI-compatible implementation is in `src/provider/openai.rs`.
- Provider catalog and selection logic live in `src/provider/catalog.rs`, `src/provider/factory.rs`, and related CLI modules like `src/cli_provider.rs` and `src/cli_model.rs`.
- `README.md` documents OpenAI-compatible provider support, including OpenRouter, Anthropic-compatible routes, OpenAI, Ollama-style endpoints, and DeepSeek-style endpoints.
- The provider path supports buffered and streaming responses and must preserve hook behavior in both paths.
- Response cleanup for provider-specific reasoning tags lives in `src/provider/think_sanitizer.rs`.
- Secret-safe logging/redaction helpers live in `src/provider/redact.rs`.

## Remote APIs And HTTP

- General HTTP calls use `reqwest`.
- Web fetch and web search tools are implemented in `src/tools/web.rs` and guarded by `src/tools/web_ssrf.rs`.
- OpenAI-compatible base URLs and model IDs are configured in `config.example.toml`.
- Provider tokens are read from config or `VULCAN_API_KEY`.
- Remote inference services are treated as API endpoints; Vulcan does not deploy inference servers.

## Daemon And Client IPC

- The daemon runtime is in `src/daemon/`.
- Client transport and auto-start behavior live in `src/client/transport.rs` and `src/client/auto_start.rs`.
- Protocol definitions and tests are in `src/daemon/protocol.rs` and `src/daemon/protocol_tests.rs`.
- Lifecycle and session management live in `src/daemon/lifecycle.rs`, `src/daemon/session.rs`, and `src/daemon/session_agent.rs`.
- Frontends are expected to talk to the daemon per `docs/adr/0001-daemon-required-frontends.md`.

## Gateway Platforms

- Gateway integration lives under `src/gateway/`.
- HTTP server routes are in `src/gateway/server.rs`.
- Inbound/outbound queues are in `src/gateway/queue.rs` and `src/gateway/outbound.rs`.
- Platform registry and rendering are in `src/gateway/registry.rs`, `src/gateway/render_registry.rs`, and `src/gateway/stream_render.rs`.
- Discord connector code is in `src/gateway/discord.rs` and uses Serenity when the gateway feature is enabled.
- Telegram connector code is in `src/gateway/telegram.rs` behind the `telegram` feature.
- Loopback testing support is in `src/gateway/loopback.rs`.

## Observability

- OpenTelemetry setup lives in `src/observability.rs`.
- Configuration is under `[observability]` in `config.example.toml`.
- OTLP HTTP export defaults to `http://localhost:4318`.
- Service name defaults to `vulcan`.
- Surface toggles include agent, hooks, tools, provider, daemon, gateway, and Symphony.
- Tracing is routed differently for TUI and non-TUI paths in `vulcan/src/main.rs`.

## Local Storage And Memory

- SQLite storage uses `rusqlite` and bundled SQLite.
- Gateway queue persistence uses SQLite-related dependencies and modules under `src/gateway/`.
- Memory code lives in `src/memory/`, including `src/memory/cortex.rs`, `src/memory/schema.rs`, and `src/memory/codec.rs`.
- Knowledge, context packs, and playbooks live in `src/knowledge/`, `src/context_pack/`, and `src/playbook/`.
- Embedding support is configured in `config.example.toml` and uses local embedding dependencies when enabled.

## OS And Developer Tooling

- Shell and PTY tools live in `src/tools/shell.rs` and use `portable-pty`.
- File tools live in `src/tools/file.rs`, `src/tools/code_edit.rs`, and `src/tools/fs_sandbox.rs`.
- Git tooling lives in `src/tools/git.rs` and uses `gix`.
- Code search and graph tooling live in `src/tools/code_search.rs`, `src/tools/code.rs`, and `src/tools/code_graph.rs`.
- LSP integration lives in `src/tools/lsp.rs` and uses `lsp-types`.

## Extension Integration

- Extension contracts live in `src/extensions/`.
- Inventory-based extension registration is wired through first-party extension crates listed in `Cargo.toml`.
- Extension manifest parsing lives in `src/extensions/manifest.rs`.
- Extension registry and store behavior lives in `src/extensions/registry.rs` and `src/extensions/store.rs`.
- Frontend extension contracts are exercised by `tests/frontend_extensions.rs`.

## CI And Repository Integrations

- GitHub Actions CI is defined in `.github/workflows/ci.yml`.
- Benchmark workflow is defined in `.github/workflows/bench.yml`.
- Dependency policy is configured through `deny.toml`.
- Unused dependency checks are handled by cargo-machete in CI.
