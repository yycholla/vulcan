//! YYC-17 PR-3: persistence for scheduler job runs.
//!
//! The job *definitions* live in `Config.scheduler` (TOML), so a
//! re-deploy with updated cron text or prompt body takes effect on
//! restart. This module is for the *mutable* counterpart: per-job
//! run history (last fire timestamp, last status, total / skipped /
//! failed fire counts) plus replacement counters used by the
//! overlap-policy gate.
//!
//! Storage is the same SQLite file the gateway queues use. The
//! scheduler holds a `SchedulerStore` that hands out short-lived
//! sync DB ops via the gateway pool's `with_init` busy_timeout, so
//! a contended writer doesn't pin the runtime thread.

use std::sync::Arc;

use anyhow::{Context, Result};
use rusqlite::{OptionalExtension, params};

use crate::memory::DbPool;

/// Status code stamped on `scheduler_runs.last_status` for the
/// most recent firing of a job. Kept narrow + symbolic so the
/// admin endpoint (PR-C-4) can present it without rendering free-
/// form prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduledFireStatus {
    /// Firing was enqueued onto the inbound queue.
    Enqueued,
    /// Firing was suppressed because the previous run is still
    /// active and `overlap_policy = "skip"`.
    Skipped,
    /// Firing was attempted but enqueueing failed (DB unavailable,
    /// queue corruption, etc.). `last_error` carries the message.
    EnqueueFailed,
}

impl ScheduledFireStatus {
    /// String form persisted in `scheduler_runs.last_status`. The
    /// `record_*` methods write the literals directly today, but
    /// the admin endpoint in PR-C-4 will format these for display.
    #[allow(dead_code)]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Enqueued => "enqueued",
            Self::Skipped => "skipped",
            Self::EnqueueFailed => "enqueue_failed",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScheduledRun {
    pub job_id: String,
    pub last_fired_at: Option<i64>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub last_inbound_id: Option<i64>,
    pub total_fires: u64,
    pub skipped_fires: u64,
    pub replaced_fires: u64,
    pub failed_fires: u64,
}

/// Thin wrapper that owns the `DbPool` handle and exposes the
/// scheduler's persistence operations as short, sync critical
/// sections.
#[derive(Clone)]
pub struct SchedulerStore {
    pool: Arc<DbPool>,
}

impl SchedulerStore {
    pub fn new(pool: DbPool) -> Self {
        Self {
            pool: Arc::new(pool),
        }
    }

    fn conn(&self) -> Result<r2d2::PooledConnection<r2d2_sqlite::SqliteConnectionManager>> {
        self.pool.get().context("scheduler DB pool checkout")
    }

    /// Record an enqueued firing. Bumps `total_fires`, sets
    /// `last_status = 'enqueued'`, and stamps the row with the
    /// inbound queue id so observability can join the two tables.
    pub fn record_enqueued(&self, job_id: &str, fired_at: i64, inbound_id: i64) -> Result<()> {
        self.record_enqueued_replacing(job_id, fired_at, inbound_id, 0)
    }

    /// Record an enqueued firing after coalescing older pending firings for the
    /// same Scheduled Job. `replaced_count` counts rows removed before enqueue.
    pub fn record_enqueued_replacing(
        &self,
        job_id: &str,
        fired_at: i64,
        inbound_id: i64,
        replaced_count: usize,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO scheduler_runs \
                 (job_id, last_fired_at, last_status, last_error, last_inbound_id, total_fires, skipped_fires, replaced_fires, failed_fires) \
             VALUES (?1, ?2, 'enqueued', NULL, ?3, 1, 0, ?4, 0) \
             ON CONFLICT(job_id) DO UPDATE SET \
                 last_fired_at = excluded.last_fired_at, \
                 last_status = excluded.last_status, \
                 last_error = NULL, \
                 last_inbound_id = excluded.last_inbound_id, \
                 total_fires = scheduler_runs.total_fires + 1, \
                 replaced_fires = scheduler_runs.replaced_fires + excluded.replaced_fires",
            params![job_id, fired_at, inbound_id, replaced_count as i64],
        )?;
        Ok(())
    }

    /// Record a firing that was skipped by overlap policy. Bumps
    /// `total_fires` + `skipped_fires` so reporting can show
    /// suppression rates without a separate query.
    pub fn record_skipped(&self, job_id: &str, fired_at: i64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO scheduler_runs \
                 (job_id, last_fired_at, last_status, total_fires, skipped_fires, replaced_fires, failed_fires) \
             VALUES (?1, ?2, 'skipped', 1, 1, 0, 0) \
             ON CONFLICT(job_id) DO UPDATE SET \
                 last_fired_at = excluded.last_fired_at, \
                 last_status = excluded.last_status, \
                 last_error = NULL, \
                 total_fires = scheduler_runs.total_fires + 1, \
                 skipped_fires = scheduler_runs.skipped_fires + 1",
            params![job_id, fired_at],
        )?;
        Ok(())
    }

    /// Record an enqueue failure. Carries the error message verbatim
    /// so operators can read what went wrong without trawling logs.
    pub fn record_enqueue_failed(&self, job_id: &str, fired_at: i64, error: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO scheduler_runs \
                 (job_id, last_fired_at, last_status, last_error, total_fires, skipped_fires, replaced_fires, failed_fires) \
             VALUES (?1, ?2, 'enqueue_failed', ?3, 1, 0, 0, 1) \
             ON CONFLICT(job_id) DO UPDATE SET \
                 last_fired_at = excluded.last_fired_at, \
                 last_status = excluded.last_status, \
                 last_error = excluded.last_error, \
                 total_fires = scheduler_runs.total_fires + 1, \
                 failed_fires = scheduler_runs.failed_fires + 1",
            params![job_id, fired_at, error],
        )?;
        Ok(())
    }

    /// Read the persisted row for a job, if any. Returns `None`
    /// for a job that has never fired.
    pub fn get(&self, job_id: &str) -> Result<Option<ScheduledRun>> {
        let conn = self.conn()?;
        let row = conn
            .query_row(
                "SELECT job_id, last_fired_at, last_status, last_error, last_inbound_id, \
                        total_fires, skipped_fires, replaced_fires, failed_fires \
                 FROM scheduler_runs WHERE job_id = ?1",
                params![job_id],
                |row| {
                    Ok(ScheduledRun {
                        job_id: row.get(0)?,
                        last_fired_at: row.get(1)?,
                        last_status: row.get(2)?,
                        last_error: row.get(3)?,
                        last_inbound_id: row.get(4)?,
                        total_fires: row.get::<_, i64>(5)?.max(0) as u64,
                        skipped_fires: row.get::<_, i64>(6)?.max(0) as u64,
                        replaced_fires: row.get::<_, i64>(7)?.max(0) as u64,
                        failed_fires: row.get::<_, i64>(8)?.max(0) as u64,
                    })
                },
            )
            .optional()
            .context("scheduler_runs SELECT")?;
        Ok(row)
    }

    /// Snapshot every job's run history. Sorted by `job_id` so the
    /// admin endpoint output is stable.
    pub fn list(&self) -> Result<Vec<ScheduledRun>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT job_id, last_fired_at, last_status, last_error, last_inbound_id, \
                    total_fires, skipped_fires, replaced_fires, failed_fires \
             FROM scheduler_runs ORDER BY job_id ASC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ScheduledRun {
                    job_id: row.get(0)?,
                    last_fired_at: row.get(1)?,
                    last_status: row.get(2)?,
                    last_error: row.get(3)?,
                    last_inbound_id: row.get(4)?,
                    total_fires: row.get::<_, i64>(5)?.max(0) as u64,
                    skipped_fires: row.get::<_, i64>(6)?.max(0) as u64,
                    replaced_fires: row.get::<_, i64>(7)?.max(0) as u64,
                    failed_fires: row.get::<_, i64>(8)?.max(0) as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("scheduler_runs collect")?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SchedulerStore {
        let pool = crate::memory::in_memory_gateway_pool().expect("pool");
        SchedulerStore::new(pool)
    }

    // YYC-17 PR-3: enqueued firing inserts a fresh row and bumps
    // counters; reading it back round-trips the fields.
    #[test]
    fn record_enqueued_inserts_and_increments() {
        let s = store();
        s.record_enqueued("daily", 100, 7).unwrap();
        let row = s.get("daily").unwrap().expect("row");
        assert_eq!(row.last_status.as_deref(), Some("enqueued"));
        assert_eq!(row.last_fired_at, Some(100));
        assert_eq!(row.last_inbound_id, Some(7));
        assert_eq!(row.total_fires, 1);
        assert_eq!(row.skipped_fires, 0);
        assert_eq!(row.failed_fires, 0);

        s.record_enqueued("daily", 200, 8).unwrap();
        let row = s.get("daily").unwrap().unwrap();
        assert_eq!(row.last_fired_at, Some(200));
        assert_eq!(row.total_fires, 2);
    }

    // YYC-17 PR-3: skipped firings increment skipped_fires + total.
    #[test]
    fn record_skipped_increments_skip_counter() {
        let s = store();
        s.record_skipped("hourly", 100).unwrap();
        s.record_skipped("hourly", 200).unwrap();
        let row = s.get("hourly").unwrap().unwrap();
        assert_eq!(row.last_status.as_deref(), Some("skipped"));
        assert_eq!(row.total_fires, 2);
        assert_eq!(row.skipped_fires, 2);
        assert_eq!(row.failed_fires, 0);
    }

    // YYC-17 PR-3: enqueue failures carry the error text.
    #[test]
    fn record_enqueue_failed_carries_error() {
        let s = store();
        s.record_enqueue_failed("nightly", 100, "queue offline")
            .unwrap();
        let row = s.get("nightly").unwrap().unwrap();
        assert_eq!(row.last_status.as_deref(), Some("enqueue_failed"));
        assert_eq!(row.last_error.as_deref(), Some("queue offline"));
        assert_eq!(row.failed_fires, 1);
        assert_eq!(row.total_fires, 1);
    }

    // YYC-17 PR-3: list returns all rows sorted by job_id.
    #[test]
    fn list_returns_jobs_sorted_by_id() {
        let s = store();
        s.record_enqueued("zeta", 1, 1).unwrap();
        s.record_enqueued("alpha", 2, 2).unwrap();
        s.record_enqueued("middle", 3, 3).unwrap();
        let rows = s.list().unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r.job_id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "middle", "zeta"]);
    }

    // YYC-17 PR-3: missing job returns None, not an error.
    #[test]
    fn get_returns_none_for_missing_job() {
        let s = store();
        assert!(s.get("never-fired").unwrap().is_none());
    }
}
