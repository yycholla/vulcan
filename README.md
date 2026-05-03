<!-- generated-by: gsd-doc-writer -->
# Vulcan

Vulcan is a Rust workspace for a local AI agent with a CLI, terminal UI, daemon backend, optional gateway connectors, and an in-tree extension system.

## Installation

Install the Rust toolchain declared by `rust-toolchain.toml`, then build the workspace:

```bash
cargo build --all-targets
```

For a release binary:

```bash
cargo build --release
```

The main binary target is `vulcan`, declared by the root package and the `vulcan` workspace member.

## Quick Start

1. Create `~/.vulcan/config.toml` from `config.example.toml`.
2. Set a provider API key in the config file or with `VULCAN_API_KEY`.
3. Start the default chat UI:

```bash
cargo run
```

4. Run a one-shot prompt when you do not need the TUI:

```bash
cargo run -- prompt "hello"
```

## Usage Examples

Start an interactive session:

```bash
cargo run -- chat
```

Resume a saved session by id:

```bash
cargo run -- session SESSION_ID
```

Run the gateway daemon after initializing gateway configuration:

```bash
cargo run --features gateway -- gateway init
cargo run --features gateway -- gateway run
```

## Workspace Layout

The root `Cargo.toml` defines a Cargo workspace. The root package `vulcan-core` contains the shared library in `src/lib.rs`, and member crates provide binaries, frontend APIs, extension macros, built-in extensions, and demo extensions.

Important entry points:

| Path | Purpose |
|------|---------|
| `src/cli.rs` | Clap command tree for the root CLI surface. |
| `src/agent/mod.rs` | Core agent loop and tool orchestration. |
| `src/tui/mod.rs` | Terminal UI runtime. |
| `src/daemon/` | Long-lived daemon process and client-facing dispatch. |
| `src/gateway/` | Optional Axum gateway routes and platform connectors. |
| `src/extensions/` | Extension registry, manifests, and lifecycle support. |
| `vulcan/` | Daemon binary crate. |
| `vulcan-tui/` | TUI frontend binary crate. |

## Documentation

- `docs/architecture/overview.md` describes major runtime components and data flow.
- `docs/guides/getting-started.md` covers first-run setup.
- `docs/guides/development.md` covers local development workflow.
- `docs/testing/overview.md` covers test commands and CI coverage.
- `docs/configuration/overview.md` covers config files, environment variables, and defaults.
- `docs/reference/api.md` covers gateway HTTP routes.
- `docs/runtime/overview.md` covers runtime module boundaries.

## License

The root Cargo package declares the project license as MIT in `Cargo.toml`.
