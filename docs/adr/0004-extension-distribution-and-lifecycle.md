# Extension Distribution as Cargo Crates and Mid-Session Lifecycle

Vulcan extensions ship as Cargo crates that link into the daemon and/or frontend binaries at build time, with `inventory` driving auto-registration. Manifest metadata is read from `[package.metadata.vulcan]` so a crate is its own source of truth for id, version, capabilities, replay safety ceiling, and required surfaces. WASM, subprocess, and native dynamic loading are deferred to a later phase. Mid-session changes follow a deliberately mixed policy: `disable` soft-drains (in-flight tool calls finish, no new dispatches, hooks fire current turn but skip on the next `BeforePrompt`); `enable` live-promotes (the daemon walks live **Sessions** and instantiates the new factory, refusing if a tool name collides with a draining extension); new manifests appear in the registry immediately and stay `Broken` until daemon restart links the code; `uninstall` is daemon-future-only with already-running Sessions retaining the extension; `vulcan extension kill <id>` is the explicit force-stop with a "may break in-flight tool calls" warning.

## Considered Options

- Cargo crates as the only distribution target; defer all dynamic loading.
- WASM-first via `wasmtime` and WIT bindings.
- Subprocess-first via line-delimited JSON over stdio (per the YYC-27 deferred design).
- Compile-time crates plus `libloading` for true dynamic Rust loading.

For mid-session lifecycle, also considered:
- Status flips applied to live Sessions immediately (force-deactivate on `disable`).
- Status flips applied only at session boundaries (no `enable` live-promotion).
- Lazy activation on first turn rather than at Session construction.

## Consequences

- Workspace becomes `vulcan-core` (lib), `vulcan-frontend-api` (lib), `vulcan` (daemon bin), `vulcan-tui` (frontend bin), plus one crate per extension. A `cargo vulcan-ext` scaffolder generates the recommended layout.
- Per-workspace `.vulcan/extensions/<id>/extension.toml` is allowed but lands as `Inactive + UntrustedSource`; activation requires `vulcan extension trust <id>` keyed by `(workspace_path_hash, extension_id, manifest_checksum)`. Workspace manifests can only reference already-linked extension code; arbitrary code from cloned repos cannot run on session_start.
- A live Session's set of active **Session Extensions** is *not* fixed for its lifetime: `enable` may attach a new one mid-session, and `disable` may put one into drain. Sessions therefore track an extension generation counter and emit a `session_extensions_changed` event so observers can re-snapshot.
- Soft-drain semantics require a `draining: bool` flag per **Session Extension**. New tool dispatches refuse with `extension draining`; in-flight calls complete and their `AfterToolCall` hooks still fire; the next `BeforePrompt` skips the extension's hooks.
- Live-promote semantics require the daemon to walk every active Session under the **Runtime Resource Pool** when a new factory becomes active. Tool name collisions between a newly-attaching extension and an already-draining one are refused; the live-promote retries (or surfaces) once drain settles.
- Phase 4 alternatives (subprocess, WASM, native) inherit the `DaemonCodeExtension` factory shape so adding them later does not invalidate the manifest schema.
