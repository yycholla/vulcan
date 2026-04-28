//! YYC-205: orchestration store + child-agent record model.
//!
//! Backing store for the multi-agent runtime described in YYC-68.
//! `spawn_subagent` (YYC-82) registers a record per child run; the
//! TUI subagent/tree views read from the same store so what they
//! show is real, not demo data.
//!
//! ## Design
//!
//! The store is a bounded ring of `ChildAgentRecord`s (most recent
//! 256 by default), keyed by `ChildAgentId`. All mutations land
//! through the store's API so the bounded eviction is centralized.
//! Reads return `Clone`d snapshots — the store never hands out
//! references that outlive the lock.
//!
//! Persistence is intentionally not part of this PR. The TUI is
//! the only consumer, and a process restart blanks the user
//! session anyway. SQLite-backed history is a follow-up if/when
//! cross-session inspection becomes a real ask.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Cap on the number of records kept in the store. Old records
/// fall off the front of the ring once the cap is exceeded — the
/// TUI only wants recent activity. Tunable per-store via
/// `OrchestrationStore::with_capacity`.
pub const ORCHESTRATION_DEFAULT_CAPACITY: usize = 256;

/// YYC-205: child-agent identity. UUID-backed so ids are unique
/// across the process even if the parent restarts mid-session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(transparent)]
pub struct ChildAgentId(pub Uuid);

impl ChildAgentId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ChildAgentId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ChildAgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// YYC-205: lifecycle state of a child agent. Mirrors the design
/// doc's enumeration; the runtime emits transitions via the
/// store's `mark_*` helpers so callers don't need to construct
/// these directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildStatus {
    Pending,
    Running,
    Blocked,
    Completed,
    Failed,
    Cancelled,
}

impl ChildStatus {
    /// Terminal statuses — once a record reaches one of these, no
    /// further transitions are allowed.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// YYC-205: snapshot of a child agent run as the TUI / parent see
/// it. All fields are owned so the store can hand out clones
/// without the caller holding a lock.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChildAgentRecord {
    pub id: ChildAgentId,
    pub parent_id: Option<ChildAgentId>,
    pub task_summary: String,
    pub status: ChildStatus,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    /// Free-form phase string the runtime updates as the child
    /// progresses (e.g. "thinking", "tool: read_file"). Optional
    /// because pending records haven't started yet.
    pub current_phase: Option<String>,
    /// Iterations the child has consumed so far. Updated in-flight
    /// when the runtime reports progress; final value persisted
    /// at terminal transition.
    pub iterations_used: u32,
    /// Hard cap the child is bounded by, mirrored from the
    /// spawn-time budget so the TUI can show fraction-used
    /// without joining against the spawn config.
    pub max_iterations: u32,
    /// YYC-211: cumulative `total_tokens` the child consumed
    /// across every provider response in its run. Reported by
    /// `mark_completed` / `mark_failed`; observability surfaces
    /// it alongside iterations.
    pub tokens_consumed: u64,
    /// Final summary text the child returned. `None` until the
    /// child reaches `Completed`.
    pub final_summary: Option<String>,
    /// Failure description if `status == Failed`. `None` otherwise.
    pub error: Option<String>,
}

/// YYC-205: orchestration store. Owned via `Arc` so the parent
/// agent, the spawn tool, and the TUI can all share one
/// authoritative view of in-flight + recent child runs.
pub struct OrchestrationStore {
    inner: Mutex<StoreInner>,
}

struct StoreInner {
    /// Insertion-ordered ring. Most recent run is the back.
    records: VecDeque<ChildAgentRecord>,
    /// Bounded length; a push past `capacity` evicts the front.
    capacity: usize,
    /// YYC-209: per-active-child cancellation tokens, keyed by id.
    /// Inserted by `register_cancel_handle` on child spawn,
    /// removed at terminal transition. `cancel(id)` looks up and
    /// fires.
    cancel_handles: HashMap<ChildAgentId, CancellationToken>,
}

impl OrchestrationStore {
    /// Build an empty store with the default capacity
    /// (`ORCHESTRATION_DEFAULT_CAPACITY`).
    pub fn new() -> Self {
        Self::with_capacity(ORCHESTRATION_DEFAULT_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(StoreInner {
                records: VecDeque::with_capacity(capacity.min(1024)),
                capacity: capacity.max(1),
                cancel_handles: HashMap::new(),
            }),
        }
    }

    /// Build a store wrapped in `Arc` for shared ownership across
    /// the parent agent + TUI + spawn tool. Convenience helper —
    /// callers can do this themselves.
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Register a new pending child run. Returns the inserted
    /// record by clone so the caller can stamp the id on whatever
    /// outbound object it wants.
    pub fn register(
        &self,
        parent_id: Option<ChildAgentId>,
        task_summary: impl Into<String>,
        max_iterations: u32,
    ) -> ChildAgentRecord {
        let record = ChildAgentRecord {
            id: ChildAgentId::new(),
            parent_id,
            task_summary: task_summary.into(),
            status: ChildStatus::Pending,
            started_at: Utc::now(),
            ended_at: None,
            current_phase: None,
            iterations_used: 0,
            max_iterations,
            tokens_consumed: 0,
            final_summary: None,
            error: None,
        };
        let snapshot = record.clone();
        let mut inner = self.inner.lock();
        if inner.records.len() >= inner.capacity {
            inner.records.pop_front();
        }
        inner.records.push_back(record);
        snapshot
    }

    /// Update a record's status. No-op when the record is missing
    /// or already terminal — terminal records are immutable so
    /// stale callers can't resurrect a finished run.
    pub fn update_status(&self, id: ChildAgentId, status: ChildStatus) {
        self.with_mut(id, |r| {
            if r.status.is_terminal() {
                return;
            }
            r.status = status;
            if status.is_terminal() {
                r.ended_at = Some(Utc::now());
            }
        });
    }

    pub fn update_phase(&self, id: ChildAgentId, phase: impl Into<String>) {
        let phase = phase.into();
        self.with_mut(id, |r| {
            if !r.status.is_terminal() {
                r.current_phase = Some(phase);
            }
        });
    }

    pub fn update_iterations(&self, id: ChildAgentId, iterations: u32) {
        self.with_mut(id, |r| {
            if !r.status.is_terminal() {
                r.iterations_used = iterations;
            }
        });
    }

    /// YYC-211: stamp the cumulative token total for an in-flight
    /// child. Pre-terminal only — once a record is terminal the
    /// final value was captured by `mark_completed` / `mark_failed`.
    pub fn update_tokens(&self, id: ChildAgentId, tokens: u64) {
        self.with_mut(id, |r| {
            if !r.status.is_terminal() {
                r.tokens_consumed = tokens;
            }
        });
    }

    /// Mark a record as completed with the child's final summary.
    /// Sets ended_at + iterations + status atomically; no-op if
    /// already terminal.
    pub fn mark_completed(
        &self,
        id: ChildAgentId,
        final_summary: impl Into<String>,
        iterations: u32,
    ) {
        let final_summary = final_summary.into();
        self.with_mut(id, |r| {
            if r.status.is_terminal() {
                return;
            }
            r.status = ChildStatus::Completed;
            r.final_summary = Some(final_summary);
            r.iterations_used = iterations;
            r.ended_at = Some(Utc::now());
        });
    }

    pub fn mark_failed(&self, id: ChildAgentId, error: impl Into<String>, iterations: u32) {
        let error = error.into();
        self.with_mut(id, |r| {
            if r.status.is_terminal() {
                return;
            }
            r.status = ChildStatus::Failed;
            r.error = Some(error);
            r.iterations_used = iterations;
            r.ended_at = Some(Utc::now());
        });
    }

    pub fn mark_cancelled(&self, id: ChildAgentId) {
        self.with_mut(id, |r| {
            if r.status.is_terminal() {
                return;
            }
            r.status = ChildStatus::Cancelled;
            r.ended_at = Some(Utc::now());
        });
    }

    /// Snapshot a single record by id.
    pub fn get(&self, id: ChildAgentId) -> Option<ChildAgentRecord> {
        let inner = self.inner.lock();
        inner.records.iter().find(|r| r.id == id).cloned()
    }

    /// Snapshot every record in insertion order (oldest first).
    /// The TUI reverses this for "newest first" display.
    pub fn list(&self) -> Vec<ChildAgentRecord> {
        let inner = self.inner.lock();
        inner.records.iter().cloned().collect()
    }

    /// Snapshot the most recent `n` records, newest first.
    pub fn recent(&self, n: usize) -> Vec<ChildAgentRecord> {
        let inner = self.inner.lock();
        inner.records.iter().rev().take(n).cloned().collect()
    }

    /// YYC-209: register the cancellation token for an in-flight
    /// child. Looked up by `cancel(id)` to fire the matching
    /// child's cancel signal. The spawn tool calls this on child
    /// start and `forget_cancel_handle` on terminal transition so
    /// the map doesn't accumulate dead tokens.
    pub fn register_cancel_handle(&self, id: ChildAgentId, token: CancellationToken) {
        let mut inner = self.inner.lock();
        inner.cancel_handles.insert(id, token);
    }

    /// YYC-209: drop a cancel handle without firing it. Called at
    /// terminal transition so the map stays bounded by the count
    /// of in-flight children, not lifetime-of-process.
    pub fn forget_cancel_handle(&self, id: ChildAgentId) {
        let mut inner = self.inner.lock();
        inner.cancel_handles.remove(&id);
    }

    /// YYC-209: cancel a specific child by id. Looks up the
    /// registered token, fires it, removes it from the map, and
    /// flips the record to `Cancelled`. Returns true when a token
    /// was registered and fired; false when the id is unknown or
    /// already terminal.
    pub fn cancel(&self, id: ChildAgentId) -> bool {
        let token = {
            let mut inner = self.inner.lock();
            inner.cancel_handles.remove(&id)
        };
        match token {
            Some(t) => {
                t.cancel();
                self.mark_cancelled(id);
                true
            }
            None => false,
        }
    }

    /// YYC-209: snapshot every record whose `parent_id` equals
    /// `parent`. Used by tree-of-thought rendering to walk the
    /// child hierarchy. Returned in insertion order.
    pub fn children_of(&self, parent: ChildAgentId) -> Vec<ChildAgentRecord> {
        let inner = self.inner.lock();
        inner
            .records
            .iter()
            .filter(|r| r.parent_id == Some(parent))
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().records.is_empty()
    }

    fn with_mut<F: FnOnce(&mut ChildAgentRecord)>(&self, id: ChildAgentId, f: F) {
        let mut inner = self.inner.lock();
        if let Some(record) = inner.records.iter_mut().find(|r| r.id == id) {
            f(record);
        }
    }
}

impl Default for OrchestrationStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> OrchestrationStore {
        OrchestrationStore::new()
    }

    #[test]
    fn register_assigns_id_and_pending_status() {
        let s = store();
        let r = s.register(None, "summarize", 8);
        assert_eq!(r.status, ChildStatus::Pending);
        assert_eq!(r.iterations_used, 0);
        assert_eq!(r.max_iterations, 8);
        assert!(r.ended_at.is_none());
        assert_eq!(s.len(), 1);
        let stored = s.get(r.id).expect("get");
        assert_eq!(stored.id, r.id);
    }

    #[test]
    fn lifecycle_transitions() {
        let s = store();
        let r = s.register(None, "task", 4);
        s.update_status(r.id, ChildStatus::Running);
        s.update_phase(r.id, "thinking");
        s.update_iterations(r.id, 2);
        let snap = s.get(r.id).unwrap();
        assert_eq!(snap.status, ChildStatus::Running);
        assert_eq!(snap.current_phase.as_deref(), Some("thinking"));
        assert_eq!(snap.iterations_used, 2);

        s.mark_completed(r.id, "done", 3);
        let snap = s.get(r.id).unwrap();
        assert_eq!(snap.status, ChildStatus::Completed);
        assert_eq!(snap.final_summary.as_deref(), Some("done"));
        assert_eq!(snap.iterations_used, 3);
        assert!(snap.ended_at.is_some());
    }

    #[test]
    fn terminal_records_are_immutable() {
        let s = store();
        let r = s.register(None, "task", 4);
        s.mark_completed(r.id, "done", 1);
        // Subsequent transitions are no-ops.
        s.update_status(r.id, ChildStatus::Running);
        s.update_phase(r.id, "should not stick");
        s.mark_failed(r.id, "should not", 99);
        let snap = s.get(r.id).unwrap();
        assert_eq!(snap.status, ChildStatus::Completed);
        assert_eq!(snap.iterations_used, 1);
        assert_eq!(snap.final_summary.as_deref(), Some("done"));
        assert!(snap.error.is_none());
    }

    #[test]
    fn ring_evicts_oldest_when_capacity_exceeded() {
        let s = OrchestrationStore::with_capacity(3);
        let a = s.register(None, "a", 1).id;
        let _b = s.register(None, "b", 1).id;
        let _c = s.register(None, "c", 1).id;
        // Cap = 3, all three present.
        assert!(s.get(a).is_some());
        let _d = s.register(None, "d", 1).id;
        // First insertion evicted.
        assert!(s.get(a).is_none(), "oldest record should be evicted");
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn recent_returns_newest_first() {
        let s = store();
        let a = s.register(None, "a", 1).id;
        let b = s.register(None, "b", 1).id;
        let c = s.register(None, "c", 1).id;
        let recent = s.recent(2);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, c);
        assert_eq!(recent[1].id, b);
        assert_eq!(s.list()[0].id, a);
    }

    #[test]
    fn cancellation_sets_terminal_status() {
        let s = store();
        let r = s.register(None, "task", 4);
        s.mark_cancelled(r.id);
        let snap = s.get(r.id).unwrap();
        assert_eq!(snap.status, ChildStatus::Cancelled);
        assert!(snap.ended_at.is_some());
    }

    // YYC-211: tokens_consumed defaults to 0 + update_tokens
    // mutates pre-terminal records only.
    #[test]
    fn update_tokens_records_pre_terminal_only() {
        let s = store();
        let r = s.register(None, "task", 4);
        assert_eq!(s.get(r.id).unwrap().tokens_consumed, 0);
        s.update_tokens(r.id, 4242);
        assert_eq!(s.get(r.id).unwrap().tokens_consumed, 4242);
        s.mark_completed(r.id, "ok", 1);
        // Post-terminal update is a no-op.
        s.update_tokens(r.id, 9_999);
        assert_eq!(s.get(r.id).unwrap().tokens_consumed, 4242);
    }

    // YYC-209: cancel(id) fires the registered token + flips the
    // record to Cancelled.
    #[test]
    fn cancel_fires_registered_token_and_marks_record() {
        let s = store();
        let r = s.register(None, "task", 4);
        let token = CancellationToken::new();
        s.register_cancel_handle(r.id, token.clone());
        assert!(s.cancel(r.id));
        assert!(token.is_cancelled());
        let snap = s.get(r.id).unwrap();
        assert_eq!(snap.status, ChildStatus::Cancelled);
    }

    // YYC-209: cancel on an unknown id returns false without
    // panicking.
    #[test]
    fn cancel_unknown_id_returns_false() {
        let s = store();
        let phantom = ChildAgentId::new();
        assert!(!s.cancel(phantom));
    }

    // YYC-209: forget_cancel_handle removes without firing.
    #[test]
    fn forget_cancel_handle_does_not_fire() {
        let s = store();
        let r = s.register(None, "task", 4);
        let token = CancellationToken::new();
        s.register_cancel_handle(r.id, token.clone());
        s.forget_cancel_handle(r.id);
        // Subsequent cancel returns false (handle gone) and the
        // token stays uncancelled.
        assert!(!s.cancel(r.id));
        assert!(!token.is_cancelled());
    }

    // YYC-209: children_of returns only direct children.
    #[test]
    fn children_of_returns_direct_descendants() {
        let s = store();
        let parent = s.register(None, "parent", 8);
        let c1 = s.register(Some(parent.id), "c1", 4);
        let c2 = s.register(Some(parent.id), "c2", 4);
        let _grandchild = s.register(Some(c1.id), "gc", 2);
        let _unrelated = s.register(None, "other", 4);
        let kids = s.children_of(parent.id);
        let ids: Vec<ChildAgentId> = kids.iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&c1.id));
        assert!(ids.contains(&c2.id));
    }

    #[test]
    fn parent_id_round_trips() {
        let s = store();
        let parent = s.register(None, "parent", 8);
        let child = s.register(Some(parent.id), "child", 4);
        let snap = s.get(child.id).unwrap();
        assert_eq!(snap.parent_id, Some(parent.id));
    }
}
