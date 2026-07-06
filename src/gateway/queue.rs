use crate::platform::OutboundAttachment;

/// Default per-row attempt cap before mark_failed routes a row to the
/// dead-letter queue (YYC-137). Operators can override via
/// `InboundQueue::with_policy`.
pub const DEFAULT_INBOUND_MAX_ATTEMPTS: u32 = 3;

/// Default staleness threshold for `recover_processing`. Rows whose
/// `last_heartbeat_at` is older than `now - this` are considered
/// crashed and reset to `pending`. Anything fresher is left running
/// (YYC-137 dedup against duplicate work after a quick worker
/// restart). 30 min picked to comfortably exceed the longest healthy
/// run_prompt turn; tunable via `with_policy`.
pub const DEFAULT_INBOUND_HEARTBEAT_STALE_SECS: i64 = 1800;

pub struct InboundQueue {
    pub(super) conn: turso::Connection,
    pub(super) max_attempts: u32,
    pub(super) heartbeat_stale_secs: i64,
}

#[derive(Debug, Clone)]
pub struct InboundRow {
    pub id: i64,
    pub platform: String,
    pub chat_id: String,
    pub user_id: String,
    pub text: String,
    pub received_at: i64,
    pub attempts: i64,
}

pub struct OutboundQueue {
    pub(super) conn: turso::Connection,
    pub(super) max_attempts: u32,
}

#[derive(Debug, Clone)]
pub struct OutboundRow {
    pub id: i64,
    pub platform: String,
    pub chat_id: String,
    pub text: String,
    pub attachments: Vec<OutboundAttachment>,
    pub enqueued_at: i64,
    pub next_attempt_at: i64,
    pub attempts: i64,
    pub state: String,
    pub last_error: Option<String>,
    /// YYC-18 PR-2a: anchor for edit-in-place streaming. When `Some`,
    /// the OutboundDispatcher routes to `Platform::edit` instead of
    /// `Platform::send`.
    pub edit_target: Option<String>,
    /// Reply / thread target on the platform side.
    pub reply_to: Option<String>,
    /// YYC-18 PR-2b: per-turn id used by the dispatcher to build the
    /// RenderKey for anchor capture. `None` for non-streaming rows.
    pub turn_id: Option<String>,
}

// Retry waits indexed by failures-so-far: 1st → 5s, 2nd → 30s, ... clamps at 7200s.
pub(super) fn outbound_backoff_secs(attempts: i64) -> i64 {
    const SCHEDULE: &[i64] = &[5, 30, 300, 1800, 7200];
    let idx = (attempts - 1).clamp(0, (SCHEDULE.len() - 1) as i64) as usize;
    SCHEDULE[idx]
}
