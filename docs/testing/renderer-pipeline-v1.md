# Renderer Pipeline v1 Manual Test Checklist

Run this checklist after the Renderer Pipeline v1 milestone is implemented and
the automated renderer tests pass.

## Setup

- [ ] Build the TUI binary from the milestone branch: `cargo build`.
- [ ] Start `vulcan tui` in a terminal that supports color and Unicode.
- [ ] Use a terminal width near 80 columns, then repeat the wrapping checks at a narrow width near 40 columns.
- [ ] Confirm the active theme is readable in the default terminal theme and `default-light`.

## Baseline Markdown

- [ ] Plain paragraphs render with the same spacing and wrapping as before the milestone.
- [ ] Headings `#` through `######` render with heading styling and visible heading markers.
- [ ] Bold, italic, strikethrough, links, and inline code render with distinct styles.
- [ ] Unclosed or malformed inline markers render as plain text rather than disappearing.
- [ ] Blank lines in model output remain intentional and do not create excessive trailing space.

## Wrapping And Unicode

- [ ] Emoji, CJK, box drawing, Nerd Font icons, and math symbols wrap at the visible terminal column.
- [ ] Wrapped words prefer word boundaries before splitting long words.
- [ ] Inline code and styled spans preserve styling across wrapped rows.
- [ ] Blockquote continuation rows keep the quote rail.
- [ ] Bullet and ordered list continuation rows align under the item text.
- [ ] Fenced code block continuation rows keep the code rail.

## Rich Markdown Slices

- [ ] Fenced code blocks render with syntax highlighting when a language is provided.
- [ ] Fenced code blocks without a language still render legibly.
- [ ] GFM tables align columns and degrade gracefully in narrow terminals.
- [ ] Task lists show checked and unchecked states clearly.
- [ ] Inline math renders or falls back without losing the TeX source.
- [ ] Display math blocks render or fall back as distinct blocks.

## Typst And Media Fallbacks

- [ ] Typst fenced blocks render a preview when the optional renderer is available.
- [ ] Typst fenced blocks show a clear text fallback when the renderer is unavailable.
- [ ] Preview cache reuse is visible on repeated renders of the same block.
- [ ] Failed preview generation does not block normal chat rendering.
- [ ] Media placeholders show useful alt text or path context.

## CLI And Non-TUI Output

- [ ] CLI output paths using rendered Markdown still produce readable plain/ANSI output.
- [ ] Copying rendered TUI text does not include extra decorative rails beyond existing behavior.
- [ ] Gateway/attachment fallback paths can consume the Render IR without TUI-only assumptions.

## Regression Checks

- [ ] Long chat transcripts still scroll smoothly.
- [ ] Existing render cache behavior remains stable when messages stream incrementally.
- [ ] Switching themes does not change structural wrapping.
- [ ] The renderer does not introduce heavy Typst/LaTeX dependencies unless the matching feature is enabled.
- [ ] `cargo test` passes, or any remaining failures are documented as unrelated baseline failures.
