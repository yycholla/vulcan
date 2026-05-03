<!-- generated-by: gsd-doc-writer -->
# Getting Started

## Prerequisites

- Rust toolchain from `rust-toolchain.toml`.
- A provider reachable through the OpenAI-compatible provider path in `src/provider/openai.rs`.
- A `~/.vulcan/config.toml` file or enough environment/config values for the selected provider.

## Installation Steps

Clone the repository and build all targets:

```bash
git clone https://github.com/yycholla/vulcan.git
cd vulcan
cargo build --all-targets
```

Create your local config file:

```bash
mkdir -p ~/.vulcan
cp config.example.toml ~/.vulcan/config.toml
```

Then edit `~/.vulcan/config.toml` for your provider, or set `VULCAN_API_KEY`.

## First Run

Start the interactive TUI:

```bash
cargo run
```

Run a one-shot prompt:

```bash
cargo run -- prompt "Summarize this repository"
```

## Common Setup Issues

| Issue | Fix |
|-------|-----|
| Missing provider credentials | Set `VULCAN_API_KEY` or add `api_key` under the active provider config. |
| Gateway command fails with missing `[gateway]` config | Run `cargo run --features gateway -- gateway init`, then review the generated gateway settings. |
| Local provider model catalog fetch fails | Set `disable_catalog = true` for local OpenAI-compatible providers that do not expose a catalog. |

## Next Steps

- Read `docs/guides/development.md` for local development commands.
- Read `docs/testing/overview.md` before changing runtime behavior.
- Read `docs/configuration/overview.md` when editing config-related code.
