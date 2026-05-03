---
title: INTEGRATIONS
last_mapped_commit: b48a9a7197a90fc5410b05ac5b66b4b2797dba6e
mapped_at: 2026-05-03
scope: full repo
---

# External Integrations

**Analysis Date:** 2026-05-03

Vulcan integrates with OpenAI-compatible model APIs, optional embedding APIs, DuckDuckGo HTML search, Discord, Telegram, OpenTelemetry collectors, GitHub Actions, local OS/process resources, and local SQLite/redb-backed stores.

## APIs & External Services

**LLM Providers:**
- OpenAI-compatible chat completions - agent model calls and streaming tool turns.
  - SDK/Client: `reqwest` from `Cargo.toml`.
  - Implementation: `src/provider/openai.rs`, `src/provider/mod.rs`, `src/agent/provider.rs`.
  - Endpoint shape: `POST {base_url}/chat/completions` in `src/provider/openai.rs`.
  - Auth: `provider.api_key` in `~/.vulcan/config.toml` or `VULCAN_API_KEY`; config source in `src/config/mod.rs` and `config.example.toml`.
  - Default example provider: OpenRouter-compatible `https://openrouter.ai/api/v1` in `config.example.toml`.
- Provider model catalogs - startup/model selection metadata from `/models`.
  - SDK/Client: `reqwest`.
  - Implementation: `src/provider/catalog.rs`, `src/cli_model.rs`, `src/cli_provider.rs`.
  - Supports OpenRouter-style rich catalogs and OpenAI-style sparse catalogs in `src/provider/catalog.rs`.
  - Auth: same provider key path as chat completions.
- OpenAI-compatible embeddings - optional semantic code search.
  - SDK/Client: `reqwest`.
  - Implementation: `src/code/embed.rs`.
  - Endpoint shape: `POST {base_url}/embeddings` in `src/code/embed.rs`.
  - Auth: `[embeddings].api_key`, `[provider].api_key`, or `VULCAN_API_KEY` through `src/code/embed.rs` and `config.example.toml`.
  - Default model example: `text-embedding-3-small` in `config.example.toml`.

**Web Search And Fetch:**
- DuckDuckGo HTML search - simple web search tool.
  - SDK/Client: `reqwest`, `scraper`, `html2text`, `percent-encoding`.
  - Implementation: `src/tools/web.rs`.
  - Endpoint: `https://html.duckduckgo.com/html/?q=...` in `src/tools/web.rs`.
  - Auth: None detected.
- Web page fetch - user-requested URL fetch with SSRF guardrails.
  - SDK/Client: `reqwest`, `html2text`.
  - Implementation: `src/tools/web.rs`, `src/tools/web_ssrf.rs`.
  - Auth: None detected.

**Gateway HTTP API:**
- Local gateway API - inbound queue, lane inspection, scheduler status, and platform webhooks.
  - SDK/Client: `axum`, `tower`, `http`.
  - Implementation: `src/gateway/server.rs`, `src/gateway/routes/inbound.rs`, `src/gateway/routes/lanes.rs`, `src/gateway/routes/scheduler.rs`, `src/gateway/routes/webhook.rs`.
  - Routes: `GET /health`, `GET /v1/lanes`, `POST /v1/inbound`, `GET /v1/scheduler`, `POST /webhook/{platform}` in `docs/reference/api.md` and `src/gateway/server.rs`.
  - Auth: `gateway.api_token` bearer token for `/v1/*`; per-platform webhook verification for `/webhook/{platform}`.

**Messaging Platforms:**
- Discord - optional gateway platform connector.
  - SDK/Client: `serenity` `0.12` with `client`, `gateway`, `http`, `model`, and `rustls_backend` features.
  - Implementation: `src/gateway/discord.rs`.
  - Auth: `gateway.discord.bot_token` in config; required when Discord is enabled by `src/gateway/discord.rs`.
  - Receive path: Serenity gateway client enqueues `InboundMessage` rows through `src/gateway/queue.rs`.
  - Send path: Serenity HTTP sends/edits outbound messages from `src/gateway/discord.rs`.
- Telegram Bot API - optional gateway platform connector behind the `telegram` feature.
  - SDK/Client: `teloxide-core` `0.13.0`.
  - Implementation: `src/gateway/telegram.rs`.
  - Auth: `gateway.telegram.bot_token`; webhook auth uses `gateway.telegram.webhook_secret` checked against `X-Telegram-Bot-Api-Secret-Token`.
  - Receive paths: long poll via `getUpdates` and webhook via `POST /webhook/telegram` in `src/gateway/telegram.rs`.
  - Send path: `send_message`, media send APIs, `get_file`, and file download through `teloxide-core` in `src/gateway/telegram.rs`.
- Loopback gateway platform - local signed test platform.
  - SDK/Client: in-tree code plus `hmac`, `sha2`, `subtle`.
  - Implementation: `src/gateway/loopback.rs`.
  - Auth: HMAC signature header checked in `src/gateway/loopback.rs` and routed by `src/gateway/routes/webhook.rs`.

**Local Developer Integrations:**
- Git - repository discovery and shell-backed git tool operations.
  - SDK/Client: `gix` for discovery; `git` binary for write operations.
  - Implementation: `src/tools/git.rs`.
  - Auth: external git configuration, SSH agent, or credential helper; no in-repo secret path detected.
- LSP servers - code intelligence via language servers.
  - SDK/Client: `lsp-types` with hand-rolled JSON-RPC framing.
  - Implementation: `src/code/lsp/mod.rs`, `src/code/lsp/requests.rs`, `src/tools/lsp.rs`.
  - Auth: None detected.
- Shell/PTY - local process execution.
  - SDK/Client: `portable-pty`, `tokio::process`.
  - Implementation: `src/tools/shell.rs`, `src/gateway/commands.rs`.
  - Auth: inherited local OS permissions and configured tool approval policy from `config.example.toml`.

## Data Storage

**Databases:**
- SQLite session and gateway store.
  - Connection: local file under `~/.vulcan/sessions.db`, with `VULCAN_HOME` override in `src/config/mod.rs`.
  - Client: `rusqlite`, `r2d2`, `r2d2_sqlite`.
  - Implementation: schema and pool in `src/memory/schema.rs`, session store in `src/memory/mod.rs`, queues in `src/gateway/queue.rs`, scheduler history in `src/gateway/scheduler_store.rs`.
  - Tables include `sessions`, `messages`, `messages_fts`, `inbound_queue`, `outbound_queue`, and `scheduler_runs` in `src/memory/schema.rs`.
- SQLite run records.
  - Connection: `~/.vulcan/run_records.db`.
  - Client: `rusqlite`.
  - Implementation: `src/run_record/mod.rs`, `src/cli_run.rs`.
- SQLite artifact store.
  - Connection: `~/.vulcan/artifacts.db`.
  - Client: `rusqlite`.
  - Implementation: `src/artifact/mod.rs`, `src/cli_artifact.rs`.
- SQLite playbook store.
  - Connection: `~/.vulcan/playbooks.db`.
  - Client: `rusqlite`.
  - Implementation: `src/playbook/mod.rs`.
- SQLite extension install state.
  - Connection: `~/.vulcan/extension_state.db`.
  - Client: `rusqlite`.
  - Implementation: `src/extensions/install_state.rs`.
- SQLite code graph store.
  - Connection: `~/.vulcan/code_graph/<sanitized-cwd>.db`.
  - Client: `rusqlite`, `tree-sitter`.
  - Implementation: `src/code/graph.rs`.
- SQLite embeddings store.
  - Connection: `~/.vulcan/embeddings/<sanitized-cwd>.db`.
  - Client: `rusqlite`, remote OpenAI-compatible embeddings endpoint.
  - Implementation: `src/code/embed.rs`.
- Cortex graph memory.
  - Connection: `~/.vulcan/cortex.redb` by default; `CortexConfig.db_path` can override in `src/config/mod.rs`.
  - Client: `cortex-memory-core` with redb-backed storage.
  - Implementation: `src/memory/cortex.rs`, `src/cli_cortex.rs`, `src/daemon/handlers/cortex.rs`.

**File Storage:**
- Local filesystem only for config, sessions, run stores, generated skill drafts, extension state, benchmark outputs, and logs.
- Config directory is `~/.vulcan/` or `VULCAN_HOME` from `src/config/mod.rs`.
- TUI logs use a file path chosen in `vulcan/src/main.rs` so tracing output does not corrupt the terminal.
- Generated skill drafts write under configured `skills_dir` and `_pending/` per `src/config/mod.rs`.
- Extension crate manifests and registrations live in first-party workspace crates such as `vulcan-ext-todo/Cargo.toml` and `vulcan-ext-todo/src/lib.rs`.

**Caching:**
- Provider model catalog cache is implemented in `src/provider/catalog.rs` and controlled by `provider.catalog_cache_ttl_hours` in `config.example.toml`.
- Code graph and embedding indexes persist as SQLite files under `~/.vulcan/code_graph/` and `~/.vulcan/embeddings/`.
- Cortex keeps a daemon-owned redb handle when enabled; daemon ownership is described in `src/memory/cortex.rs`, `src/runtime_pool.rs`, and `docs/adr/0001-daemon-required-frontends.md`.
- No Redis, Memcached, or external cache service detected.

## Authentication & Identity

**Auth Provider:**
- Custom local/provider auth.
  - Provider API auth uses bearer token headers in `src/provider/openai.rs`, with key source `provider.api_key` or `VULCAN_API_KEY`.
  - Provider profile presets mention `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `DEEPSEEK_API_KEY`, `GROQ_API_KEY`, `TOGETHER_API_KEY`, `FIREWORKS_API_KEY`, or `VULCAN_API_KEY` as user-facing hints in `src/cli_provider.rs`; canonical fallback in code is `VULCAN_API_KEY`.
  - CLI auth/config helpers live in `src/cli_auth.rs`, `src/cli_config.rs`, and `src/config_registry.rs`.
- Gateway API auth.
  - `/v1/*` uses `Authorization: Bearer <gateway.api_token>` with constant-time comparison in `src/gateway/server.rs`.
  - `/health` is unauthenticated in `src/gateway/server.rs`.
  - `/webhook/{platform}` bypasses bearer auth and delegates platform verification through `src/gateway/routes/webhook.rs`.
- Platform auth.
  - Discord uses `gateway.discord.bot_token` in `src/gateway/discord.rs`.
  - Telegram uses `gateway.telegram.bot_token` and optional `gateway.telegram.webhook_secret` in `src/gateway/telegram.rs`.
  - Loopback uses HMAC signing in `src/gateway/loopback.rs`.

## Monitoring & Observability

**Error Tracking:**
- No Sentry, Rollbar, Honeycomb, or hosted error-tracking SDK detected.
- Structured errors use `anyhow` and `thiserror` across the codebase, with provider-specific error mapping in `src/provider/mod.rs`.

**Logs:**
- Logging framework: `tracing` and `tracing-subscriber`.
- Entrypoint setup: `vulcan/src/main.rs`.
- TUI mode logs to a file; one-shot/non-TUI paths log to stderr per `CLAUDE.md` and `vulcan/src/main.rs`.
- Provider wire logging is configurable by `provider.debug` and redacted by `src/provider/redact.rs`.

**Telemetry:**
- OpenTelemetry traces and metrics are optional and off by default in `config.example.toml`.
- Implementation: `src/observability.rs`.
- Exporter: OTLP/HTTP via `opentelemetry-otlp`.
- Default endpoint: `http://localhost:4318`, deriving `/v1/traces` and `/v1/metrics` in `src/observability.rs` and `config.example.toml`.
- Surface toggles include agent, hooks, tools, provider, daemon, gateway, and Symphony in `config.example.toml`.

## CI/CD & Deployment

**Hosting:**
- No cloud hosting platform is configured in the repository.
- Runtime target is local binary execution via Cargo or release binary from `Cargo.toml` and `README.md`.
- Optional gateway daemon binds to configured local address from `[gateway]` in `config.example.toml`.

**CI Pipeline:**
- GitHub Actions CI in `.github/workflows/ci.yml`.
  - Jobs: fmt, clippy, test, coverage, feature-powerset, deny, machete.
  - Tools: `dtolnay/rust-toolchain`, `Swatinem/rust-cache`, `taiki-e/install-action`, `EmbarkStudios/cargo-deny-action`, `bnjbvr/cargo-machete`.
  - Permissions: read-only `contents: read` by default.
- GitHub Actions benchmark workflow in `.github/workflows/bench.yml`.
  - Runs `cargo bench`, `vulcan-soak`, `scripts/median-of-3.py`, and `scripts/bench-diff.py`.
  - Uploads benchmark artifacts with `actions/upload-artifact`.
  - Downloads prior baseline with `dawidd6/action-download-artifact`.

## Environment Configuration

**Required env vars:**
- `VULCAN_API_KEY` - required only when the active provider requires auth and no config key is present; used by `src/config/mod.rs` and documented in `docs/configuration/overview.md`.
- `VULCAN_HOME` - optional override for the config/data directory in `src/config/mod.rs`.
- `VULCAN_LOG` - optional tracing filter used by `vulcan/src/main.rs`.
- `VULCAN_PLATFORM`, `VULCAN_CHAT_ID`, `VULCAN_USER_ID` - set by gateway shell command execution in `src/gateway/commands.rs`.
- `VULCAN_EXT_COMPACT_SUMMARY_MODE` - extension test/demo behavior toggle in `vulcan-ext-compact-summary/src/lib.rs`.
- `CARGO_TARGET_DIR`, `CARGO_BIN_EXE_vulcan`, and `BENCH_TURNS` - development/test/CI variables used by `tests/support/mod.rs`, benchmark files under `benches/`, and `.github/workflows/bench.yml`.

**Secrets location:**
- Runtime secrets belong in `~/.vulcan/config.toml` or environment variables; `config.example.toml` documents keys but does not contain usable secrets.
- Gateway secrets live under `[gateway]`, `[gateway.discord]`, and `[gateway.telegram]` in `~/.vulcan/config.toml` when configured.
- GitHub Actions use the platform-provided `GITHUB_TOKEN`; no custom GitHub secrets are referenced by `.github/workflows/ci.yml` or `.github/workflows/bench.yml`.
- No checked-in `.env` files were detected by the mapper; local `config.toml` exists and was not read because it may contain secrets.

## Webhooks & Callbacks

**Incoming:**
- `POST /webhook/{platform}` - platform webhook ingress in `src/gateway/routes/webhook.rs`.
  - Registered platforms are resolved through `src/gateway/registry.rs`.
  - Webhook verification delegates to `Platform::verify_webhook` implementations such as `src/gateway/telegram.rs` and `src/gateway/loopback.rs`.
  - Verified messages enqueue into the SQLite inbound queue through `src/gateway/queue.rs`.
- `POST /v1/inbound` - local authenticated JSON ingress for gateway processing in `src/gateway/routes/inbound.rs`.
- Telegram webhook secret header `X-Telegram-Bot-Api-Secret-Token` is verified in `src/gateway/telegram.rs`.
- Loopback webhook HMAC signatures are generated/verified in `src/gateway/loopback.rs`.

**Outgoing:**
- OpenAI-compatible chat completions and model catalogs from `src/provider/openai.rs` and `src/provider/catalog.rs`.
- OpenAI-compatible embeddings from `src/code/embed.rs`.
- DuckDuckGo HTML search and generic URL fetch from `src/tools/web.rs`.
- Discord outbound sends/edits through Serenity in `src/gateway/discord.rs`.
- Telegram outbound sends/edits/media/file download through teloxide-core in `src/gateway/telegram.rs`.
- OTLP trace/metric export to an OpenTelemetry collector from `src/observability.rs`.
- GitHub Actions artifact upload/download in `.github/workflows/ci.yml` and `.github/workflows/bench.yml`.

---

*Integration audit: 2026-05-03*
