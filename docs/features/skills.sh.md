---
title: Skills — Feature Specification
created: 2026-04-29
type: feature
tags: [skills, installation, lifecycle]
---

# Skills

Skills are reusable, task-specific knowledge packages that the agent loads at boot and can reference during a session. They provide structured guidance for common workflows (debugging, code review, git commits, etc.)

## Installation

Skills are installed **as skills** — flat markdown files with YAML frontmatter. They are not extensions (see [Promotion below](#promotion-skill--extension) for when a skill graduates to an extension). Skills live in one of two locations:

| Source | Path | Priority |
|--------|------|----------|
| Bundled (shipped with binary) | `<repo>/skills/` | Fallback (low) |
| User (configurable) | `~/.vulcan/skills/` | Primary (high) |

At startup, the `SkillRegistry` checks `~/.vulcan/skills/` first. If the directory doesn't exist, it falls back to the bundled `skills/` directory shipped in the repo.

### Adding a skill

```bash
# Place a markdown file in the user skills directory
cp my-skill.md ~/.vulcan/skills/

# Or place in the repo's bundled skills (ships with the binary)
cp my-skill.md ./skills/
```

Each skill file follows this format:

```markdown
---
name: my-skill
description: What this skill does
triggers: ["trigger phrase 1", "trigger phrase 2"]
---

## Instructions

Step-by-step guidance the agent follows when this skill is matched.
```

### Auto-creation

When the agent completes a task involving 5+ tool calls, it may prompt the user to save the interaction as a new skill. Drafts are written to:

```
~/.vulcan/skills/_pending/<name>.md
```

The user reviews and moves them to `~/.vulcan/skills/` to activate.

## Lifecycle

```
[File on disk] → load at boot → SkillRegistry → SkillsHook (BeforePrompt injection) → Agent loop
```

1. **Load** — `SkillRegistry::new()` reads all `.md` files from the skills directory at construction time.
2. **Surface** — `SkillsHook` injects the skill listing into every LLM turn via `BeforePrompt` at `InjectPosition::AfterSystem`.
3. **Match** — The LLM matches user intent against skill names, descriptions, and triggers, then follows the skill's instructions.
4. **Evolve** — If a skill proves consistently useful, it can be promoted to an **extension** (see Promotion below).

## Promotion: Skill → Extension

A skill that demonstrates sustained utility can be promoted to an **extension** — a more structured, code-backed component rather than a flat markdown file.

### Promotion criteria

A skill is a candidate for extension promotion when:

- **Frequency** — The skill is triggered across multiple sessions, not just once.
- **Complexity** — The skill's instructions have grown beyond what flat markdown comfortably expresses (branching logic, conditional steps, external data).
- **Tool coupling** — The skill routinely integrates with specific tools or APIs in a patterned way.

### Promotion path

```
Skill (markdown) → Draft extension → Code extension
```

| Stage | Format | What changes |
|-------|--------|--------------|
| **Skill** | `.md` file in `~/.vulcan/skills/` | Human-authored markdown, loaded and injected as prompt text |
| **Draft extension** | `.md` with extended frontmatter (`extension: true`, `depends: [...]`, `config_schema: {...}`) | Richer metadata; still prompt-based but with structured parameters |
| **Extension** | Rust module + hook handler | Compiled into the binary or loaded dynamically; full programmatic control over when and how it activates |

### Configuration

Skills can specify extension-promotion preferences in their frontmatter:

```markdown
---
name: deploy-check
description: Pre-deployment validation checklist
triggers: ["deploy", "release", "ship"]
extension: candidate          # mark as promotion candidate
extension_confidence: 0.7    # 0.0–1.0 — how well it meets criteria
---
```

The `extension_confidence` field is a hint to the promotion system — it's not authoritative. The final decision to promote is made by the user, not by automation.
