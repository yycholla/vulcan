<!-- generated-by: gsd-doc-writer -->
# vulcan

`vulcan` is the daemon binary crate for the Vulcan workspace.

Part of the [Vulcan](../README.md) workspace.

## Build

```bash
cargo build -p vulcan --bin vulcan
```

## Role

This crate packages the daemon-facing binary surface separately from the root `vulcan-core` library package. Use the root README and `docs/architecture/overview.md` for the shared runtime architecture.

## Testing

Run workspace tests from the repository root:

```bash
cargo test
```
