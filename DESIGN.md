---
name: Vulcan TUI
description: Fast terminal agent workspace with ASCII-canvas discipline.
colors:
  ink: "#15130f"
  paper: "#f2eee5"
  muted: "#8a8478"
  faint: "#e2dccd"
  slate: "#c8c2b5"
  red: "#d63b2f"
  yellow: "#e8b43c"
  blue: "#2b4fa8"
  green: "#3f7a4f"
  dracula-fg: "#f8f8f2"
  dracula-comment: "#6272a4"
  dracula-cyan: "#8be9fd"
  dracula-green: "#50fa7b"
  dracula-orange: "#ffb86c"
  dracula-pink: "#ff79c6"
  dracula-purple: "#bd93f9"
  dracula-red: "#ff5555"
  dracula-yellow: "#f1fa8c"
typography:
  display:
    fontFamily: "terminal monospace, IBM Plex Mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"
    fontSize: "terminal cell"
    fontWeight: 700
    lineHeight: 1
    letterSpacing: "normal"
  title:
    fontFamily: "terminal monospace, IBM Plex Mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"
    fontSize: "terminal cell"
    fontWeight: 700
    lineHeight: 1
    letterSpacing: "normal"
  body:
    fontFamily: "terminal monospace, IBM Plex Mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"
    fontSize: "terminal cell"
    fontWeight: 400
    lineHeight: 1
    letterSpacing: "normal"
  label:
    fontFamily: "terminal monospace, IBM Plex Mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"
    fontSize: "terminal cell"
    fontWeight: 700
    lineHeight: 1
    letterSpacing: "normal"
rounded:
  none: "0px"
spacing:
  hairline: "1 terminal cell"
  compact: "2 terminal cells"
  rail: "28 terminal cells"
components:
  frame:
    textColor: "{colors.muted}"
    rounded: "{rounded.none}"
    padding: "1 terminal row title bar"
  prompt-row:
    textColor: "{colors.ink}"
    rounded: "{rounded.none}"
    padding: "1 divider row, 1+ input rows, 1 hint row"
  tool-card:
    textColor: "{colors.muted}"
    rounded: "{rounded.none}"
    padding: "border-only, 2 terminal cell body indent"
---

# Design System: Vulcan TUI

## 1. Overview

**Creative North Star: "The ASCII Workbench"**

Vulcan is a terminal-first agent workspace where text is the interface material. The visual system should feel fast, clean, and attractive without turning the main view into a dashboard. It borrows from Lazygit, Neovim, Zed, Raycast, Superhuman, Stripe developer surfaces, and stark ASCII-canvas systems: compact rhythm, visible state, sharp hierarchy, and minimal ornament.

The TUI is allowed to be dense, but density must be earned by workflow value. The main screen prioritizes focused chat, live tool state, diffs, safety prompts, and the prompt row. Secondary modes such as trading floor, tiled mesh, telemetry, and multi-session views may become busy because monitoring is their job.

The system rejects neon hacker styling, generic SaaS admin panels, toy chatbot affordances, decorative gradients, glossy elevation, and color used as decoration.

**Key Characteristics:**

- Terminal-native, keyboard-first, text-led.
- Foreground emphasis over painted backgrounds.
- Box drawing, brackets, rails, glyphs, and monospace rhythm as the core visual grammar.
- Readable compactness, not cramped opacity.
- Portable principles for a future web UI, without forcing web conventions into the TUI.

## 2. Colors

The palette is restrained and terminal-aware: inherit the user's background where possible, use a Bauhaus light palette for the historical theme, and reserve saturated colors for semantic state.

### Primary

- **Forge Red** (`#d63b2f`): Primary accent, prompt caret, ticker label, critical action emphasis, and destructive safety choices.

### Secondary

- **Blueprint Blue** (`#2b4fa8`): System messages, primary safety choices, list markers in the light theme, and structured informational emphasis.

### Tertiary

- **Tool Yellow** (`#e8b43c`): Running tool calls, warning capacity state, and active work indicators.
- **Result Green** (`#3f7a4f`): Successful tool results, success state, and completed work indicators.

### Neutral

- **Ink** (`#15130f`): Default-light foreground and role labels.
- **Paper** (`#f2eee5`): Historical Bauhaus canvas, kept as a reference token. Current TUI backgrounds should normally inherit the terminal.
- **Muted Ash** (`#8a8478`): Borders, secondary labels, timestamps, inactive metadata, and dimmed separators.
- **Faint Ash** (`#e2dccd`): Inset trace tint and low-emphasis reasoning surfaces.
- **Tool Slate** (`#c8c2b5`): Historical tool-card chrome reference.

### Named Rules

**The Terminal Inheritance Rule.** The default surface inherits the terminal background. Paint foreground, borders, glyphs, and state first.

**The Semantic Saturation Rule.** Red, yellow, blue, and green are state colors. They should signal action, status, risk, or live work, not decoration.

**The Dracula Exception.** Dracula is a built-in validation theme, not the product's default visual identity. Use it to prove theme coverage, not to make dark mode the baseline.

## 3. Typography

**Display Font:** Terminal monospace, with IBM Plex Mono as the preferred web and mockup analogue.
**Body Font:** Terminal monospace, with IBM Plex Mono as the preferred web and mockup analogue.
**Label/Mono Font:** Same family.

**Character:** The system is monospaced by construction. Hierarchy comes from weight, glyphs, uppercase labels, spacing, line structure, and semantic color rather than font pairing.

### Hierarchy

- **Display** (bold, terminal cell, 1 line-height): Rare ASCII titles, view names, and future web UI hero-like terminal artifacts.
- **Headline** (bold, terminal cell, 1 line-height): Frame title bars such as `AGENT · SINGLE STACK` and major view headers.
- **Title** (bold, terminal cell, 1 line-height): Section headers, active session labels, palette titles, picker headers, and tool-card names.
- **Body** (regular, terminal cell, 1 line-height): Chat text, markdown output, tool args, previews, and queue entries. Long prose should wrap cleanly and preserve code formatting.
- **Label** (bold, terminal cell, uppercase when useful): Status pills, mode pills, keyboard hints, model status, and semantic state labels.

### Named Rules

**The One Typeface Rule.** Do not introduce decorative display faces into the TUI. Future web UI may use IBM Plex Mono or another disciplined monospace, but the product language remains text-first.

**The Glyphs Are UI Rule.** `▓▓`, `▣`, `▎`, `▒`, `█`, `┌`, `│`, `└`, `✓`, `✗`, and `●` are interface primitives. Use them consistently.

## 4. Elevation

The TUI is flat by design. It does not use shadows or blurred layers. Depth is conveyed through structure: thick borders, box drawing, rails, dividers, indentation, muted text, and semantic foreground color. This keeps copy-paste clean and respects terminal themes.

### Named Rules

**The No Shadow Rule.** Do not add fake elevation to terminal surfaces. Use borders, rows, rails, and spacing.

**The Background Restraint Rule.** Avoid painted regions unless a component has a proven structural need. Current implementation intentionally omits backgrounds for frames, prompt rows, body text, and most chrome.

## 5. Components

### Buttons

The TUI does not use graphical buttons. Commands appear as inline keyboard affordances and bracketed action pills.

- **Shape:** Rectilinear text tokens, no radius (`0px`).
- **Primary:** Filled bracket text such as `[A] ALLOW`, colored with Blueprint Blue or another semantic foreground.
- **Hover / Focus:** Keyboard focus should be represented by position, bold weight, selection marker, or reversed row where already established.
- **Secondary / Ghost / Tertiary:** Unfilled pill text with body foreground or muted foreground.

### Chips

- **Style:** Bracketed uppercase text, e.g. `[CHAT]`, `[TICKER]`, safety options, mode labels, and status labels.
- **State:** Use bold foreground for active state. Use muted foreground for inactive metadata. Do not rely on color alone when the state is critical.

### Cards / Containers

- **Corner Style:** Square terminal geometry (`0px`).
- **Background:** Inherit the terminal background by default.
- **Shadow Strategy:** No shadows.
- **Border:** Thick outer frames for views; box-drawing cards for tool calls; left rails for message bodies and reasoning traces.
- **Internal Padding:** One to two terminal cells. Tool-card body text is indented two cells under the border.

### Inputs / Fields

- **Style:** Prompt row is a three-part terminal control: divider row, input row, hint row.
- **Focus:** The cursor is the focus indicator. Use `█` at rest and `▒` with slow blink while thinking.
- **Error / Disabled:** Use Forge Red with explicit text. Do not use red without an accompanying label or glyph.

### Navigation

- **Style:** View switching is numeric and keyboard-first: Single Stack, Split Sessions, Tiled Mesh, Tree of Thought, Trading Floor.
- **Active State:** Bold label, active marker, or accent foreground.
- **Secondary Navigation:** Slash command palette, model picker, provider picker, and session rail should use list rows with stable columns and predictable markers.

### Markdown Renderer

- **Headings:** Preserve markdown markers (`#`, `##`) and apply bold role styling.
- **Code Blocks:** Prefix each line with ` │` and preserve monospace rhythm.
- **Inline Code:** Use semantic inline-code foreground, not a filled badge.
- **Blockquotes:** Use `▎` rail plus italic muted treatment.
- **Lists:** Use `•` or ordered prefixes with list-marker color.

### Tool Cards

- **Character:** Compact, inspectable, border-only records of work.
- **Header:** Tool name, params summary, status glyph, status label, and elapsed time when available.
- **States:** Running uses Tool Yellow, success uses Result Green, failure uses Forge Red.
- **Body:** Args, result metadata, output preview, and elision should stay readable at narrow widths.

## 6. Do's and Don'ts

### Do:

- **Do** inherit the terminal background unless a component has a documented structural reason to paint.
- **Do** use text, box drawing, glyphs, and stable columns as the primary UI material.
- **Do** keep Single Stack focused and let dense dashboards live in secondary views.
- **Do** use red, yellow, blue, and green for semantic state: risk, running, info, success.
- **Do** make tool calls, diffs, safety pauses, queue state, lineage, and model context auditable at the right moment.
- **Do** keep prompt-row behavior polished at narrow widths, including wrapped input and visible cursor state.
- **Do** preserve practical terminal accessibility: readable contrast, non-color state cues, visible selection, and legible truncation.

### Don't:

- **Don't** make the default experience a busy SaaS admin panel.
- **Don't** use neon hacker terminal styling.
- **Don't** make the product feel like a toy chatbot.
- **Don't** use decorative gradients, glossy surfaces, glass effects, or shadow-based elevation.
- **Don't** use generic identical card grids for core TUI surfaces.
- **Don't** use side-stripe card accents as decoration. Message rails are allowed because they are structural role markers.
- **Don't** hide hierarchy behind cramped ncurses density. Compact is good only when it remains navigable and readable.
- **Don't** treat Dracula or any dark theme as the product default by category reflex.
