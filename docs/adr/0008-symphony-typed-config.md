# ADR-0008: Symphony Typed Config

## Status

Accepted

## Context

Symphony workflow front matter must configure task sources, polling, workspaces, hooks, agent behavior, and Codex launch settings without letting later orchestrator code repeatedly parse raw YAML. The service also needs defensive poll-time reload behavior: a bad edit to `WORKFLOW.md` should pause future dispatches rather than crash active automation.

## Decision

Layer a typed `symphony::config` view over the workflow front matter. `ConfigView` builds an `EffectiveConfig` with defaults, numeric string coercion, environment variable resolution, and path normalization relative to the repository root. Startup validation returns clear `ConfigError` paths for missing task-source kind, source-specific config, required source auth, and Codex command.

Poll-time reload uses `LastKnownGoodConfig`. Valid reloads replace the effective config; invalid reloads keep the previous value and retain the validation error for operator-visible diagnostics. This is a defensive reload contract, not a filesystem watcher.

## Consequences

- Orchestrator, task-source, workspace, and runner slices can depend on typed config instead of raw YAML lookups.
- Unknown workflow front-matter keys remain tolerated by the workflow loader; the typed view reads only the keys it owns.
- Source adapters can add source-specific validation incrementally behind the typed task-source section.
- In-flight workers do not restart when config changes; reloads affect future dispatch decisions.
