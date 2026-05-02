---
name: skills
description: "Skill for the Skills area of vulcan. 32 symbols across 2 files."
---

# Skills

32 symbols | 2 files | Cohesion: 93%

## When to Use

- Working with code in `src/`
- Understanding how load_body, with_dirs, default_for work
- Modifying skills-related functionality

## Key Files

| File | Symbols |
|------|---------|
| `src/skills/mod.rs` | load_body, with_dirs, default_for, list, strip_frontmatter (+26) |
| `src/agent/mod.rs` | skills |

## Entry Points

Start here when exploring this area:

- **`load_body`** (Function) — `src/skills/mod.rs:60`
- **`with_dirs`** (Function) — `src/skills/mod.rs:78`
- **`default_for`** (Function) — `src/skills/mod.rs:104`
- **`list`** (Function) — `src/skills/mod.rs:199`
- **`skills`** (Function) — `src/agent/mod.rs:780`

## Key Symbols

| Symbol | Type | File | Line |
|--------|------|------|------|
| `load_body` | Function | `src/skills/mod.rs` | 60 |
| `with_dirs` | Function | `src/skills/mod.rs` | 78 |
| `default_for` | Function | `src/skills/mod.rs` | 104 |
| `list` | Function | `src/skills/mod.rs` | 199 |
| `skills` | Function | `src/agent/mod.rs` | 780 |
| `strip_frontmatter` | Function | `src/skills/mod.rs` | 369 |
| `write_skill` | Function | `src/skills/mod.rs` | 387 |
| `discovers_folder_layout_skill` | Function | `src/skills/mod.rs` | 398 |
| `body_loads_lazily_after_metadata_discovery` | Function | `src/skills/mod.rs` | 414 |
| `parses_optional_spec_fields` | Function | `src/skills/mod.rs` | 429 |
| `missing_frontmatter_is_skipped_with_warning` | Function | `src/skills/mod.rs` | 448 |
| `directory_without_skill_md_is_ignored` | Function | `src/skills/mod.rs` | 461 |
| `later_dir_shadows_earlier_dir_by_name` | Function | `src/skills/mod.rs` | 471 |
| `nonexistent_dir_is_silently_skipped` | Function | `src/skills/mod.rs` | 505 |
| `default_for_includes_project_root_paths` | Function | `src/skills/mod.rs` | 513 |
| `project_agents_skill_overrides_user_skill_with_same_name` | Function | `src/skills/mod.rs` | 546 |
| `discovers_skills_in_collection_layout` | Function | `src/skills/mod.rs` | 610 |
| `collection_recursion_is_capped_at_one_level` | Function | `src/skills/mod.rs` | 656 |
| `strip_frontmatter_returns_body_only` | Function | `src/skills/mod.rs` | 684 |
| `discover` | Function | `src/skills/mod.rs` | 122 |

## How to Explore

1. `gitnexus_context({name: "load_body"})` — see callers and callees
2. `gitnexus_query({query: "skills"})` — find related execution flows
3. Read key files listed above for implementation details
