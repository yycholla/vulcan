# Next Steps

Ordered plan as of 2026-07-06. Sequences the in-flight Turso migration first
(finish what's started), then the product gaps from [comparison.md](comparison.md)
and [recommendations.md](recommendations.md) in priority order. Standalone
feature tasks are slotted where they fit.

## Phase A — Finish the Turso migration (GH #704)

Done: seam (Phase 0), playbook, artifact, run_record, code/embed. Pattern proven
(async trait or cfg-gated struct + rusqlite default + turso impl behind
`turso-backend` + selector; callers await). Remaining, in ascending risk:

1. **code/graph** — concrete struct (~15 methods), no vectors. Same cfg-gated
   `db_*` helper approach as code/embed. Callers: impact ×2, knowledge, tools ×2.
2. **extensions/state** — the scope pattern; `ExtensionStateScope` holds the
   connection. Port scope + its get/set/checkpoint methods.
3. **extensions/install_state** — biggest cascade: forces the ~60-function sync
   extension registry loader async. Budget for it; do it in its own PR.
4. **gateway trio + memory-FTS finale** — inbound/outbound queue + scheduler +
   memory share one r2d2 `DbPool`; they move together. This is where FTS5
   (`messages_fts` virtual table + 3 triggers) becomes Turso native FTS
   (`CREATE INDEX ... USING fts`, `fts_match`/`fts_score`, no triggers) and
   `db_blocking`/r2d2/`spawn_blocking` get deleted — the real prize.
5. **Cutover** — drop `rusqlite`/`r2d2`/`r2d2_sqlite`, remove the feature flag,
   make Turso the only backend.

## Phase B — One-wire product gaps (already-landed substrate)

These map to wiki recommendations and filed GH issues; each is small.

6. **Visible memory/profile contract** (rec P2) — `vulcan memory show/add/remove`,
   `vulcan profile show/edit`, bounded session-start injection, no autonomous
   writes. Highest user-facing value; Vulcan has the storage, lacks the surface.
7. **Skill draft promotion loop** (rec P3) — `_pending/` list/show-source/
   promote/delete; promoted skills immutable. Makes `auto_create_skills` safe.
8. **Task #1 — real mock replay** (GH #285 area) — re-drive a turn against
   recorded provider/tool outputs, then expose `vulcan replay mock`. Builds on
   the now-async run_record + artifact stores.

## Phase C — Security & gateway hardening (before more autonomy)

9. **Persistent-action provenance** (rec P4) — stamp source surface/session/run/
   channel + action digest on memory writes, skill promotions, scheduler jobs,
   extension installs, gateway file patches; one-shot approval from untrusted
   surfaces.
10. **Gateway session controls** (rec P5) — crisp status/stop/approve/deny/new/
    retry/usage/resume before any new connector. Wire the `approval.*` daemon
    stubs here (they're still METHOD_NOT_IMPLEMENTED).
11. **One sandbox backend** (rec P6) — Docker first, only when a workflow needs
    it. Don't add SSH in the same slice.

## Phase D — Feature completeness (larger, own sessions)

12. **Task #3 — WASM `register_tool` ABI** (GH #548/#273) — schema + guest invoke
    entrypoint + registry dispatch; closes Extension System v1.
13. **Artifact/replay depth** (rec P8) — subagent transcript handles, TUI child-
    session timeline (builds on the orchestration tree just shipped), artifact
    viewer commands, read-only replay drift checks.
14. **Task #5 — typst + terminal images** (GH #702) — compiler dep behind
    `render-typst`, kitty/sixel widget behind `render-images`.
15. **Task #6 — vulcan-tui extraction** (GH #560/#664) — daemon-client frontend
    loop; drive `src/tui` against the daemon.
16. **Provider capability matrix** (rec P7) — track streaming/tool-call/JSON/
    tokens/context/pricing/error-shape per provider/model; enables safe fallback.

## Not now (per comparison.md)

Hermes' full connector count; multiple sandbox backends at once; autonomous
skill/memory rewriting without a review gate; a separate Kanban DB (artifacts +
Symphony can represent tasks).
