<!-- generated-by: gsd-doc-writer -->
# vulcan-tui

`vulcan-tui` is the TUI frontend binary crate for Vulcan.

Part of the [Vulcan](../README.md) workspace.

## Build

```bash
cargo build -p vulcan-tui
```

## Role

This crate keeps the terminal frontend binary packaging separate from the shared runtime. The reusable TUI implementation lives under `src/tui/` in the root library crate.

## Testing

```bash
cargo test -p vulcan-tui
```
