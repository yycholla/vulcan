# Next Steps

Ordered plan as of 2026-07-06. Sequences the in-flight Turso migration first
(finish what's started), then the product gaps from [comparison.md](comparison.md)
and [recommendations.md](recommendations.md) in priority order. Standalone
feature tasks are slotted where they fit.

## Phase A — Finish the Turso migration (GH #704)

Done: seam (Phase 0), playbook, artifact, run_record, code/embed. Pattern proven
(async trait or cfg-gated struct + rusqlite default + turso impl behind
`turso-backend` + selector; callers await). Remaining, in ascending risk:

1. **code/graph** — **DEFERRED / keep on rusqlite.** Attempted and reverted:
   `CodeGraph::open` going async forces `ToolRegistry::new*` async, which cascades
   into **20+ call sites** (mostly tests) plus the agent build — a disproportionate
   blast radius for **zero migration value** (no FTS, no `db_blocking`/r2d2; it's a
   plain `Mutex<Connection>`). Port only if the tool-registry constructor is
   independently made async, or refactor the code-graph tools to open the graph
   lazily inside their async `call()` (an `Arc<OnceCell<CodeGraph>>`) so the
   registry constructor stays sync. Not worth it before the value-bearing stores.
2. **extensions/state** — the scope pattern; `ExtensionStateScope` holds the
   connection. Same caution as code/graph: its get/set/checkpoint callers run in
   the largely-sync WASM extension host, so check for a constructor cascade first.
3. **extensions/install_state** — biggest cascade: forces the ~60-function sync
   extension registry loader async. Budget for it; do it in its own PR.
4. **memory-FTS store — now independently portable.** ✅ The shared-file blocker
   is **resolved**: the gateway queues were split out of `sessions.db` into
   `gateway.db` (commit "Split gateway queues out of sessions.db"), so
   `SessionStore` owns `sessions.db` alone. Next: port `SessionStore` to Turso —
   the FTS thesis. ~14 methods; cfg-gate the `conn` field + `db_*` bodies like
   code/embed. The FTS conversion: `messages_fts USING fts5(...)` + 3 triggers →
   `CREATE INDEX messages_fts ON messages USING fts(content)` (Turso auto-maintains
   it, delete the triggers); `WHERE messages_fts MATCH ?1` → `WHERE fts_match(content, ?1)`;
   `bm25(messages_fts)` (lower=better) → `fts_score(content, ?1)` (higher=better,
   flip the ORDER BY); and `sanitize_fts_query` must join tokens with explicit
   `AND` (Turso/Tantivy defaults to OR). Constructor cascade: `try_new`/`in_memory`
   become async → callers in agent, daemon session handlers, RecallHook, runtime_pool,
   cli_cortex, extensions/api, tests. Verify RecallHook + `session.search` (GH #703)
   return the same top hits on a fixture corpus.
5. **gateway queues + scheduler** — now that they own `gateway.db`, port them to
   Turso and delete `db_blocking`/r2d2/`spawn_blocking`. No FTS here (pure CRUD),
   so this is the "delete the async scaffolding" win. They still share `gateway.db`,
   so port inbound/outbound/scheduler together.
6. **Cutover** — drop `rusqlite`/`r2d2`/`r2d2_sqlite`, remove the feature flag,
   make Turso the only backend. (Requires all stores ported, or a permanent
   rusqlite carve-out for code/graph + code/embed if those stay unported.)

### Lesson from the migration so far

Trait-based application stores (playbook, artifact, run_record) port cleanly — a
trait already isolates the backend and callers already `.await`. **Concrete
structs whose constructor is called from many sync sites** (code/graph via the
tool registry; likely extensions/state via the WASM host) cascade the async
conversion far beyond the store itself. For those, either introduce a lazy-open
handle so the sync constructor survives, or accept a permanent rusqlite carve-out.
The value-bearing stores (FTS + `db_blocking` deletion) are all in the gateway
trio — prioritize those over faithful driver-swaps of the code-intelligence
stores.

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
