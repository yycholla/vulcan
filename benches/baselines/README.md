# Bench baselines

This directory intentionally contains no machine-generated baseline JSON
files. Baselines are **not committed** — they are hardware-dependent and
would churn this directory every time someone benched on a new machine.

## Local workflow

1. On `main` (or whichever branch you trust as the baseline), generate one:
   ```
   oo cargo bench
   oo cargo run --profile release-dist --bin vulcan-soak --features bench-soak -- --turns 100
   cp target/bench-results.json /tmp/vulcan-baseline.json
   ```
2. On your branch, regenerate and diff:
   ```
   oo cargo bench
   oo cargo run --profile release-dist --bin vulcan-soak --features bench-soak -- --turns 100
   python3 scripts/bench-diff.py target/bench-results.json /tmp/vulcan-baseline.json
   ```
3. Exit code 0 means no metric exceeded the configured threshold (default
   1.20×; soak group can be tightened with `--soak-threshold 1.10` once the
   numbers stabilize).

The bench writer (`benches/common/results.rs`) is read-modify-write, so each
of the three surfaces (`tui_render`, `agent_core`, `soak`) appends into the
same `target/bench-results.json`. To start from a clean artifact, delete the
file first: `rm -f target/bench-results.json`.

## CI workflow

`.github/workflows/bench.yml` (when added) stores baselines as workflow
artifacts keyed by the latest `main` SHA. PR runs download the latest
baseline artifact and diff against it. Nightly runs on `main` refresh the
baseline.

## Refreshing a stale baseline

If a legitimate optimization moves a metric, the nightly run on `main`
refreshes the artifact automatically the next night. To accelerate, re-run
the workflow on `main` manually: `gh workflow run bench.yml`.

Locally: re-run step 1 of the local workflow above.
