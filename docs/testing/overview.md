<!-- generated-by: gsd-doc-writer -->
# Testing

## Test Framework and Setup

Vulcan uses Rust's built-in test harness, `tokio` async tests, integration tests under `tests/`, snapshot tests through `insta`, benchmark targets through `divan`, and CI execution through `cargo-nextest`.

Install the pinned Rust toolchain from `rust-toolchain.toml`. CI also installs `cargo-nextest`, `cargo-llvm-cov`, `cargo-hack`, `cargo-deny`, and `cargo-machete`.

## Running Tests

Run the default test suite:

```bash
cargo test
```

Run gateway-specific tests:

```bash
cargo test --features gateway gateway::
```

Run one test by name substring:

```bash
cargo test config_loads
```

Run doc tests:

```bash
cargo test --doc
cargo test --doc --features gateway
```

Run benchmarks:

```bash
cargo bench
cargo run --release --bin vulcan-soak --features bench-soak -- --turns 20
```

## Writing New Tests

- Unit tests usually live in the same module under `#[cfg(test)]`.
- Integration tests live under `tests/`.
- Shared integration helpers live in `tests/support/mod.rs`.
- Snapshot-related tests use the `insta` dependency declared in `Cargo.toml`.
- Gateway route tests use Axum routers directly through `build_router(` and `tower::ServiceExt`.

## Coverage Requirements

No hard coverage threshold is configured in repository files. The `coverage` CI job runs `cargo llvm-cov nextest --all-features --workspace --lcov --output-path lcov.info` on pushes and prints a summary with `cargo llvm-cov report --summary-only`.

## CI Integration

`.github/workflows/ci.yml` runs:

| Job | Main checks |
|-----|-------------|
| `fmt` | `cargo fmt --all -- --check` |
| `clippy` | `cargo clippy --all-targets --all-features` |
| `test` | build default/gateway targets, build the `vulcan` binary crate, run nextest default/gateway suites, and run doc tests |
| `coverage` | cargo-llvm-cov on pushes |
| `feature-powerset` | cargo-hack feature powerset checks |
| `deny` | cargo-deny supply-chain checks |
| `machete` | unused dependency detection |

`.github/workflows/bench.yml` runs `cargo bench` and the `vulcan-soak` binary for benchmark regression tracking.
