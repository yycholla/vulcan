//! YYC-17 PR-3: persistence for scheduler job runs.
//!
//! The job *definitions* live in `Config.scheduler` (TOML), so a
//! re-deploy with updated cron text or prompt body takes effect on
//! restart. This module is for the *mutable* counterpart: per-job
//! run history (last fire timestamp, last status, total / skipped /
//! failed fire counts) plus the running-flag the overlap-policy
//! gate consults.
//!
//! Storage is the same SQLite file the gateway queues use.

/// Status code stamped on `scheduler_runs.last_status` for the
/// most recent scheduler lifecycle event for a job. Kept narrow +
/// symbolic so the admin endpoint can present it without rendering
/// free-form prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduledFireStatus {
    /// Firing was enqueued onto the inbound queue.
    Enqueued,
    /// Firing completed successfully and produced a reply.
    Completed,
    /// Firing reached the worker pipeline but the daemon/agent run failed.
    Failed,
    /// Firing was suppressed because the previous run is still
    /// active and `overlap_policy = "skip"`.
    Skipped,
    /// Firing was attempted but enqueueing failed (DB unavailable,
    /// queue corruption, etc.). `last_error` carries the message.
    EnqueueFailed,
}

impl ScheduledFireStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Enqueued => "enqueued",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::EnqueueFailed => "enqueue_failed",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScheduledRun {
    pub job_id: String,
    pub last_fired_at: Option<i64>,
    pub last_finished_at: Option<i64>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub last_inbound_id: Option<i64>,
    pub total_fires: u64,
    pub skipped_fires: u64,
    pub failed_fires: u64,
    pub completed_fires: u64,
    pub active_fires: u64,
}

/// Thin wrapper around the scheduler's Turso connection.
#[derive(Clone)]
pub struct SchedulerStore {
    pub(super) conn: turso::Connection,
}
