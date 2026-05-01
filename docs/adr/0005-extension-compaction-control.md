# Extension Control Over Session Compaction

Extensions can shape how a **Session**'s history compacts but cannot break the invariant that **Session History** must remain valid for the next provider request. The `on_session_before_compact` event accepts three outcomes: `Continue` (default — built-in compaction runs), `Block { reason }` (extension vetoes compaction this round; the **Turn Runner** continues without summarizing), and `RewriteHistory(Vec<Message>)` (extension supplies its own compacted history, replacing the built-in summary). `RewriteHistory` is a *separate* outcome from `RewriteMessages` (which lives on `on_context` and is transient per-prompt) so transient prompt rewrites and durable history rewrites stay structurally distinct. Every `RewriteHistory` value runs through a validation pass — at least one System message present, no orphan `tool_call_id`s, length monotonically less than input — and on validation failure the daemon logs a warning and falls back to built-in compaction. When context-overflow is imminent and an extension has returned `Block`, the daemon overrides the veto with built-in compaction and emits a `compaction_forced` event for observability.

## Considered Options

- Observe-only events (`on_session_compact` after the fact, no veto, no rewrite).
- Veto only (`Block { reason }`, no rewrite).
- Rewrite only (`RewriteHistory`, no veto).
- Veto + rewrite with shared outcome variant (re-using `RewriteMessages` for both transient `on_context` rewrites and durable compaction rewrites).
- Veto + rewrite with separate outcome variants and an override-on-overflow safety net (chosen).

## Consequences

- The validation pass in the daemon is load-bearing: a buggy extension's `RewriteHistory` cannot place the session into a state where the next provider call fails on tool-message invariants. Validation lives next to the existing `Turn Runner` compaction code so both paths share a normalization step.
- The `compaction_forced` event is observable through the standard **Turn Event** stream so frontends can surface "compaction occurred despite extension veto" in transcripts and `vulcan extension audit`.
- An extension that wants to *also* observe its own compaction (e.g. for snapshotting) registers both `on_session_before_compact` and `on_session_compact` handlers; the after-event always fires, regardless of which path produced the compacted history.
- Reusing `RewriteMessages` for compaction was tempting but rejected: a future reader stepping through the agent loop should be able to tell at a glance whether a hook is mutating the wire payload (transient) or rewriting Session History (durable). Separate variants make that explicit.
- The override-on-overflow path means a misbehaving extension that always returns `Block` cannot deadlock a long Session — the user's prompt eventually exceeds context, the daemon override fires, and the Session stays usable. The override is logged so the user can identify and disable the offending extension.
