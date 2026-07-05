<!-- generated-by: gsd-doc-writer -->

# Development

## Local Setup

Use the repository root as the working directory. The workspace root `Cargo.toml` owns shared dependency resolution for the root library crate and all member crates.

```bash
git clone https://github.com/yycholla/vulcan.git
cd vulcan
cargo build --all-targets
```

The repo also ships a Nix dev shell:

```bash
direnv allow
# or: nix develop
```

The tracked `.envrc` defaults to `use flake`, which works with stock direnv and is faster when your system enables `nix-direnv`. If you run lorri locally, either use it directly:

```bash
lorri daemon
lorri shell --flake .
```

Or opt this checkout into lorri-backed direnv by putting this line in the ignored `.envrc.local`:

```bash
use lorri
```

Copy `config.example.toml` to `~/.vulcan/config.toml` for manual CLI/TUI runs. Most tests construct temporary config directories and do not need a real provider key.

## Build Commands

| Command                                        | Description                                                                                |
| ---------------------------------------------- | ------------------------------------------------------------------------------------------ |
| `nix develop`                                  | Enter the pinned Rust/tooling shell from `flake.nix`.                                      |
| `nix build`                                    | Build the packaged `vulcan` binary with crane.                                             |
| `nix run . -- prompt "text"`                   | Run one prompt through the flake app.                                                      |
| `nix flake check`                              | Build the package and run the formatter check.                                             |
| `cargo build --all-targets`                    | Compile the workspace targets, including tests and benches that are regular build targets. |
| `cargo build --all-targets --features gateway` | Compile the gateway-enabled surface.                                                       |
| `cargo build --release`                        | Build an optimized release binary with the root release profile.                           |
| `cargo run`                                    | Start the default chat/TUI command.                                                        |
| `cargo run -- prompt "text"`                   | Run one prompt without the TUI.                                                            |
| `cargo run --features gateway -- gateway run`  | Run the gateway daemon with the `gateway` feature enabled.                                 |

## Code Style

Rust formatting and linting are configured through the Rust toolchain and Cargo lints:

| Tool       | Config                                | Command                                     |
| ---------- | ------------------------------------- | ------------------------------------------- |
| rustfmt    | `rust-toolchain.toml`, `rustfmt.toml` | `cargo fmt --all -- --check`                |
| clippy     | `Cargo.toml`, `clippy.toml`           | `cargo clippy --all-targets --all-features` |
| cargo-deny | `deny.toml`                           | `cargo deny check`                          |

The root manifest denies unsafe code at the Rust lint level.

## Branch Conventions

This repository uses Graphite for stacked PRs. Branches should stay issue-scoped and stack cleanly for GitHub review.

## PR Process

- Start from a GitHub issue or an explicitly agreed task scope.
- Keep changes focused to the affected runtime or doc area.
- Run the relevant Cargo verification before publishing.
- Use Graphite for stacked PR publication and restacking.
- Include issue-closing metadata where the PR is intended to close an issue.
