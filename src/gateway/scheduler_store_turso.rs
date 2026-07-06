//! Turso backend for [`SchedulerStore`] (GH #704). Same async surface
//! as the rusqlite impl in `scheduler_store.rs`; bodies run directly on
//! the shared `gateway.db` turso connection — no pool, no checkout.

use anyhow::Result;

use super::scheduler_store::{ScheduledFireStatus, ScheduledRun, SchedulerStore};

fn opt_str(v: Option<&str>) -> turso::Value {
    v.map(str::to_string).into()
}

fn run_from_row(row: &turso::Row) -> Result<ScheduledRun> {
    let total: i64 = row.get(6)?;
    let skipped: i64 = row.get(7)?;
    let failed: i64 = row.get(8)?;
    let completed: i64 = row.get(9)?;
    let active: i64 = row.get(10)?;
    Ok(ScheduledRun {
        job_id: row.get(0)?,
        last_fired_at: row.get(1)?,
        last_finished_at: row.get(2)?,
        last_status: row.get(3)?,
        last_error: row.get(4)?,
        last_inbound_id: row.get(5)?,
        total_fires: total.max(0) as u64,
        skipped_fires: skipped.max(0) as u64,
        failed_fires: failed.max(0) as u64,
        completed_fires: completed.max(0) as u64,
        active_fires: active.max(0) as u64,
    })
}

impl SchedulerStore {
    pub fn new(conn: turso::Connection) -> Self {
        Self { conn }
    }

    pub async fn record_enqueued(
        &self,
        job_id: &str,
        fired_at: i64,
        inbound_id: i64,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO scheduler_runs \
                     (job_id, last_fired_at, last_finished_at, last_status, last_error, last_inbound_id, \
                      total_fires, skipped_fires, failed_fires, completed_fires, active_fires) \
                 VALUES (?1, ?2, NULL, ?3, NULL, ?4, 1, 0, 0, 0, 1) \
                 ON CONFLICT(job_id) DO UPDATE SET \
                     last_fired_at = excluded.last_fired_at, \
                     last_finished_at = NULL, \
                     last_status = excluded.last_status, \
                     last_error = NULL, \
                     last_inbound_id = excluded.last_inbound_id, \
                     total_fires = scheduler_runs.total_fires + 1, \
                     active_fires = scheduler_runs.active_fires + 1",
                turso::params_from_iter([
                    turso::Value::from(job_id.to_string()),
                    turso::Value::from(fired_at),
                    turso::Value::from(ScheduledFireStatus::Enqueued.as_str().to_string()),
                    turso::Value::from(inbound_id),
                ]),
            )
            .await?;
        Ok(())
    }

    pub async fn record_skipped(&self, job_id: &str, fired_at: i64) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO scheduler_runs \
                     (job_id, last_fired_at, last_finished_at, last_status, last_error, total_fires, \
                      skipped_fires, failed_fires, completed_fires, active_fires) \
                 VALUES (?1, ?2, ?2, ?3, NULL, 1, 1, 0, 0, 0) \
                 ON CONFLICT(job_id) DO UPDATE SET \
                     last_fired_at = excluded.last_fired_at, \
                     last_finished_at = excluded.last_finished_at, \
                     last_status = excluded.last_status, \
                     last_error = NULL, \
                     total_fires = scheduler_runs.total_fires + 1, \
                     skipped_fires = scheduler_runs.skipped_fires + 1",
                turso::params_from_iter([
                    turso::Value::from(job_id.to_string()),
                    turso::Value::from(fired_at),
                    turso::Value::from(ScheduledFireStatus::Skipped.as_str().to_string()),
                ]),
            )
            .await?;
        Ok(())
    }

    pub async fn record_enqueue_failed(
        &self,
        job_id: &str,
        fired_at: i64,
        error: &str,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO scheduler_runs \
                     (job_id, last_fired_at, last_finished_at, last_status, last_error, total_fires, \
                      skipped_fires, failed_fires, completed_fires, active_fires) \
                 VALUES (?1, ?2, ?2, ?3, ?4, 1, 0, 1, 0, 0) \
                 ON CONFLICT(job_id) DO UPDATE SET \
                     last_fired_at = excluded.last_fired_at, \
                     last_finished_at = excluded.last_finished_at, \
                     last_status = excluded.last_status, \
                     last_error = excluded.last_error, \
                     total_fires = scheduler_runs.total_fires + 1, \
                     failed_fires = scheduler_runs.failed_fires + 1",
                turso::params_from_iter([
                    turso::Value::from(job_id.to_string()),
                    turso::Value::from(fired_at),
                    turso::Value::from(ScheduledFireStatus::EnqueueFailed.as_str().to_string()),
                    turso::Value::from(error.to_string()),
                ]),
            )
            .await?;
        Ok(())
    }

    pub async fn record_completed(&self, job_id: &str, finished_at: i64) -> Result<()> {
        self.finish_by_job_id(
            job_id,
            finished_at,
            ScheduledFireStatus::Completed,
            None,
            true,
        )
        .await
    }

    pub async fn record_completed_by_inbound(
        &self,
        inbound_id: i64,
        finished_at: i64,
    ) -> Result<()> {
        self.finish_by_inbound(
            inbound_id,
            finished_at,
            ScheduledFireStatus::Completed,
            None,
            true,
        )
        .await
    }

    pub async fn record_run_failed(
        &self,
        job_id: &str,
        finished_at: i64,
        error: &str,
    ) -> Result<()> {
        self.finish_by_job_id(
            job_id,
            finished_at,
            ScheduledFireStatus::Failed,
            Some(error),
            false,
        )
        .await
    }

    pub async fn record_run_failed_by_inbound(
        &self,
        inbound_id: i64,
        finished_at: i64,
        error: &str,
    ) -> Result<()> {
        self.finish_by_inbound(
            inbound_id,
            finished_at,
            ScheduledFireStatus::Failed,
            Some(error),
            false,
        )
        .await
    }

    pub async fn reset_active_fires(&self) -> Result<usize> {
        let n = self
            .conn
            .execute(
                "UPDATE scheduler_runs SET active_fires = 0 WHERE active_fires > 0",
                (),
            )
            .await?;
        Ok(n as usize)
    }

    pub async fn has_active_runs(&self, job_id: &str) -> Result<bool> {
        let mut rows = self
            .conn
            .query(
                "SELECT active_fires FROM scheduler_runs WHERE job_id = ?1",
                (job_id.to_string(),),
            )
            .await?;
        let active: i64 = match rows.next().await? {
            Some(row) => row.get(0)?,
            None => 0,
        };
        Ok(active.max(0) > 0)
    }

    async fn finish_by_job_id(
        &self,
        job_id: &str,
        finished_at: i64,
        status: ScheduledFireStatus,
        error: Option<&str>,
        completed: bool,
    ) -> Result<()> {
        self.conn
            .execute(
                "UPDATE scheduler_runs SET \
                     last_finished_at = ?2, \
                     last_status = ?3, \
                     last_error = ?4, \
                     failed_fires = failed_fires + ?5, \
                     completed_fires = completed_fires + ?6, \
                     active_fires = CASE WHEN active_fires > 0 THEN active_fires - 1 ELSE 0 END \
                 WHERE job_id = ?1",
                turso::params_from_iter([
                    turso::Value::from(job_id.to_string()),
                    turso::Value::from(finished_at),
                    turso::Value::from(status.as_str().to_string()),
                    opt_str(error),
                    turso::Value::from(if completed { 0i64 } else { 1 }),
                    turso::Value::from(if completed { 1i64 } else { 0 }),
                ]),
            )
            .await?;
        Ok(())
    }

    async fn finish_by_inbound(
        &self,
        inbound_id: i64,
        finished_at: i64,
        status: ScheduledFireStatus,
        error: Option<&str>,
        completed: bool,
    ) -> Result<()> {
        self.conn
            .execute(
                "UPDATE scheduler_runs SET \
                     last_finished_at = ?2, \
                     last_status = ?3, \
                     last_error = ?4, \
                     failed_fires = failed_fires + ?5, \
                     completed_fires = completed_fires + ?6, \
                     active_fires = CASE WHEN active_fires > 0 THEN active_fires - 1 ELSE 0 END \
                 WHERE last_inbound_id = ?1",
                turso::params_from_iter([
                    turso::Value::from(inbound_id),
                    turso::Value::from(finished_at),
                    turso::Value::from(status.as_str().to_string()),
                    opt_str(error),
                    turso::Value::from(if completed { 0i64 } else { 1 }),
                    turso::Value::from(if completed { 1i64 } else { 0 }),
                ]),
            )
            .await?;
        Ok(())
    }

    pub async fn get(&self, job_id: &str) -> Result<Option<ScheduledRun>> {
        let mut rows = self
            .conn
            .query(
                "SELECT job_id, last_fired_at, last_finished_at, last_status, last_error, last_inbound_id, \
                        total_fires, skipped_fires, failed_fires, completed_fires, active_fires \
                 FROM scheduler_runs WHERE job_id = ?1",
                (job_id.to_string(),),
            )
            .await?;
        match rows.next().await? {
            Some(row) => Ok(Some(run_from_row(&row)?)),
            None => Ok(None),
        }
    }

    pub async fn list(&self) -> Result<Vec<ScheduledRun>> {
        let mut rows = self
            .conn
            .query(
                "SELECT job_id, last_fired_at, last_finished_at, last_status, last_error, last_inbound_id, \
                        total_fires, skipped_fires, failed_fires, completed_fires, active_fires \
                 FROM scheduler_runs ORDER BY job_id ASC",
                (),
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(run_from_row(&row)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> SchedulerStore {
        let db = crate::db::open_database_in_memory().await.unwrap();
        let conn = crate::db::connect_database(&db).await.unwrap();
        super::super::queue_turso::initialize_gateway_db(&conn)
            .await
            .unwrap();
        SchedulerStore::new(crate::db::connect_database(&db).await.unwrap())
    }

    #[tokio::test]
    async fn completion_by_inbound_clears_active_fire() {
        let s = store().await;
        s.record_enqueued("daily", 100, 7).await.unwrap();
        assert!(s.has_active_runs("daily").await.unwrap());

        s.record_completed_by_inbound(7, 150).await.unwrap();
        let row = s.get("daily").await.unwrap().expect("row");
        assert_eq!(row.last_status.as_deref(), Some("completed"));
        assert_eq!(row.completed_fires, 1);
        assert_eq!(row.active_fires, 0);
    }
}
