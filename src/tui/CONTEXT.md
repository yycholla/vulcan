# TUI — Context

Terminal UI. Holds the Agent in `Arc<tokio::sync::Mutex<Agent>>` for the whole session — never construct a fresh Agent per prompt.

TUI mode logs to file (so `tracing` doesn't splat the screen); one-shot mode logs to stderr.

## Glossary

_Stub — populate via `/grill-with-docs` when area-specific terms accumulate._
