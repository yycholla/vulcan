# Tools — Context

Tool trait and dispatch. Runs `BeforeToolCall` (block / replace args) → execute → `AfterToolCall` (replace result).

Today `Tool::call` returns `Result<String>`. Master plan target: `ToolResult { output, media, is_error }` — tracked in GitHub Issues, natural next structural change.

## Glossary

_Stub — populate via `/grill-with-docs` when area-specific terms accumulate._
