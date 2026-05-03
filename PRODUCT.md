# Product

## Register

product

## Users

Vulcan is primarily designed for its maintainer's daily terminal workflow, while remaining understandable and trustworthy for open source contributors and advanced developer users. Users are comfortable in terminals, editors, and code review tools. They want a fast agent workspace that exposes enough state to stay in control without turning the default screen into a cockpit.

The current design focus is the Rust TUI. Future web UI work should inherit the same product principles, but should not force web-dashboard assumptions back into the terminal.

## Product Purpose

Vulcan is a terminal-first AI agent for focused coding and operational work. It combines chat, tool execution, file edits, provider switching, session history, safety prompts, and telemetry in a single fast interface.

Success means the user can understand what the agent is doing, intervene quickly, resume prior work, inspect changes, and keep momentum without losing context. The interface should make power visible when needed, but keep the main path clean.

## Brand Personality

Fast, clean, attractive.

Vulcan should feel rigorous, sharp, and tool-like: closer to Lazygit, Neovim, Zed, Raycast, Superhuman, and Stripe's developer surfaces than a consumer chatbot. The desired aesthetic can borrow from light, monospaced ASCII-canvas systems: dense textual texture, strong contrast, minimal ornament, and compact rhythm.

The product should feel powerful without performative complexity. It should be comfortable with terminal-native density, but every dense view must still have a purpose.

## Anti-references

The default experience should not look like a busy SaaS admin panel, a neon hacker terminal, a toy chatbot, or a generic dashboard template. Busy dashboards are allowed for secondary views such as telemetry, trading-floor layouts, or multi-session monitoring, but they should not define the main screen.

Cramped ncurses-style density is acceptable when it is deliberate, navigable, and readable. It is not acceptable when it hides hierarchy, overloads the prompt, or makes agent state harder to audit.

Avoid decorative gradients, glossy effects, heavy elevation, generic card grids, and color used as decoration rather than state.

## Design Principles

1. Default to focus, reveal density on demand.
2. Treat text as the primary interface material.
3. Preserve terminal fluency: fast scanning, keyboard-first operation, predictable layout, minimal latency.
4. Make agent state auditable: tool calls, diffs, safety pauses, session lineage, and model context should be visible at the right moment.
5. Keep visual decisions portable enough that a future web UI can share the same product language without copying terminal constraints literally.

## Accessibility & Inclusion

No formal accessibility target is set yet.

For the TUI, use practical terminal accessibility as the floor: readable contrast, no essential color-only meaning, clear focus and selection states, legible truncation, reduced-motion-friendly behavior, and layouts that remain usable on common terminal sizes.

For a future web UI, revisit this section before implementation and set a stricter target appropriate to browser interfaces.
