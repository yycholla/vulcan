---
title: CONVENTIONS
last_mapped_commit: f5e07ca31dd7a4ac794b9838884bfdad74a7e37c
mapped_at: 2026-05-03
scope: full repo
---

# Conventions

Vulcan favors explicit Rust modules, daemon-owned runtime resources, hook-mediated behavior, and tests near the contracts they protect.

## Rust Style

- Rust edition 2024 is used across the workspace through `Cargo.toml`.
- Formatting is enforced by `rustfmt.toml`.
- Clippy configuration lives in `clippy.toml`.
- Public APIs use typed modules and domain names rather than generic utility buckets.
- Async code is built on Tokio and usually returns `Result` types suitable for CLI or daemon boundaries.

## Error Handling

- Application paths commonly use `anyhow` for contextual errors.
- Domain-specific errors use typed error crates such as `thiserror` where clearer boundaries are useful.
- Secret-bearing values use `secrecy` where appropriate.
- Provider logging and request handling should pass through redaction helpers in `src/provider/redact.rs`.
- User-facing errors should preserve enough command/session/provider context to debug without exposing secrets.

## Configuration

- Configuration shape lives under `src/config/`.
- CLI helpers for config concerns live in `src/cli_config.rs`.
- Example defaults are maintained in `config.example.toml`.
- Config changes should consider daemon config-watch behavior in `src/daemon/config_watch.rs`.
- Telemetry, provider, gateway, tool approval, compaction, memory, and TUI settings all have explicit config sections.

## Hook Invariants

- Hook contracts are centralized in `src/hooks/mod.rs`.
- Built-in hooks should be registered consistently in long-lived agents.
- `BeforePrompt` injections should remain transient and avoid mutating persistent session history.
- Blocking hook events should respect first non-continue behavior.
- Tool hooks should wrap both pre-call argument handling and post-call result replacement.
- Provider changes must keep buffered and streaming hook paths aligned.

## Daemon And Frontend Conventions

- Frontend work should align with daemon-required direction in `docs/adr/0001-daemon-required-frontends.md`.
- Long-lived runtime resources belong in the daemon or shared runtime pool, not per-prompt frontend construction.
- TUI mode logs to a file so tracing does not corrupt the terminal.
- One-shot CLI paths can log to stderr.
- Client behavior should use `src/client/` rather than duplicating transport code.

## Tooling Conventions

- Tools live under `src/tools/` and should participate in the shared registry in `src/tools/mod.rs`.
- File operations must honor sandboxing helpers in `src/tools/fs_sandbox.rs`.
- Shell and PTY tools should preserve auditability and approval behavior.
- Web tools should use SSRF protection from `src/tools/web_ssrf.rs`.
- Tool UI output should be renderable by TUI tool cards and replayable where possible.

## TUI Conventions

- TUI state and rendering should stay separated where existing modules already do so.
- Terminal-native rendering is preferred over painted background surfaces.
- Keybindings and slash commands should flow through the existing TUI command layers.
- Model/provider switching belongs in picker and slash-command surfaces rather than separate one-off tools.
- UI changes should account for session persistence, tool activity, and streaming output.

## Extension Conventions

- Extension architecture follows the daemon/frontend split documented in `src/extensions/CONTEXT.md`.
- Extension manifests are parsed through `src/extensions/manifest.rs`.
- Policy and verification belong under `src/extensions/policy.rs` and `src/extensions/verify.rs`.
- New first-party extensions should follow the existing `vulcan-ext-*` crate shape.
- Frontend capabilities should be represented through `vulcan-frontend-api/` contracts.

## Documentation Conventions

- Broad domain language belongs in `CONTEXT.md`, `CONTEXT-MAP.md`, and per-module `CONTEXT.md` files.
- Cross-cutting decisions belong in ADRs under `docs/adr/`.
- Keep issue and roadmap planning aligned with GitHub Issues as the source of truth.
- Avoid broad rename-only churn around remaining historical `ferris` references unless already touching a relevant file.
