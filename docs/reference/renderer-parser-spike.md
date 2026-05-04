# Renderer Parser Spike

Issue: #585

## Decision

Use `pulldown-cmark` as the first real Markdown parser behind Vulcan's owned
Render IR.

Rationale:

- `pulldown-cmark` is a low-overhead event parser with CommonMark and targeted
  GFM extensions for tables, task lists, and strikethrough.
- Keeping parser output in Vulcan's Render IR preserves control over chat
  wrapping, theme roles, cache ownership, blockquote/list rails, and future
  Typst/math fallback behavior.
- `tui-markdown` is deferred because it owns too much of the terminal rendering
  path for the current chat renderer.
- `comrak` is deferred until Vulcan needs AST-heavy transforms that are awkward
  to express from an event stream.

## Observed Behavior

- CommonMark headings, paragraphs, blockquotes, ordered/unordered lists, rules,
  fenced code blocks, inline emphasis, strong text, links, inline code, and
  strikethrough map into `RenderBlock`/`Inline`.
- GFM task-list markers map to plain inline prefixes: `[x] ` and `[ ] `.
- GFM tables map to `RenderBlock::Table` and currently render as conservative
  pipe rows. Column alignment and richer narrow-terminal behavior belong to the
  table rendering slice.
- Nested lists are represented as nested list blocks. Rich indentation and
  continuation styling remain renderer responsibilities.

## Performance Notes

Command:

```sh
rtk cargo run --release --bin tui-render-bench
```

Local result from this branch:

```text
messages: 50000
window: 40 lines @ 100 columns
first visible_lines: 173.455961ms total_lines=350000 rendered_blocks=50000 materialized_lines=40
cached tail visible_lines: avg=171.905989ms min=167.586041ms max=209.181925ms materialized_lines=40
mutated tail visible_lines: 167.335387ms rerendered_blocks=50000 materialized_lines=40
```

The parser integration did not introduce heavyweight renderer dependencies.
The existing benchmark still exercises full transcript materialization and shows
all 50k blocks rerendering after a tail mutation; improving that cache
invalidation behavior is outside this parser spike.
