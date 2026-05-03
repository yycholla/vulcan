<!-- generated-by: gsd-doc-writer -->
# vulcan-frontend-api

`vulcan-frontend-api` provides frontend extension registration API types for Vulcan extension authors.

Part of the [Vulcan](../README.md) workspace.

## Build

```bash
cargo build -p vulcan-frontend-api
```

## Role

This crate is the shared contract layer for frontend extension metadata and registration. Runtime extension discovery and lifecycle handling live in `src/extensions/` in the root library crate.

## Testing

```bash
cargo test -p vulcan-frontend-api
```
