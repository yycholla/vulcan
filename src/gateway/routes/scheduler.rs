//! YYC-17 PR-4: GET /v1/scheduler — observability snapshot of the
//! cron scheduler.
//!
//! Returns a `{ "jobs": [...] }` array merging the declared job
//! definitions from config with the latest persisted run history
//! from `scheduler_runs` and the next upcoming firing computed
//! from each job's cron expression. Jobs that have never fired
//! still appear (with `run: null`); rows in `scheduler_runs`
//! whose `job_id` no longer matches any configured job are
//! ignored — config is the source of truth for which jobs exist.

use std::str::FromStr;

use axum::Json;
use axum::extract::State;
use chrono::Utc;
use serde::Serialize;

use crate::config::SchedulerJobConfig;
use crate::gateway::scheduler_store::ScheduledRun;
use crate::gateway::server::AppState;

#[derive(Debug, Serialize)]
struct JobStatus<'a> {
    id: &'a str,
    name: &'a str,
    enabled: bool,
    cron: &'a str,
    timezone: &'a str,
    platform: &'a str,
    lane: &'a str,
    /// Unix seconds of the next firing time, or `None` when the
    /// cron expression has no upcoming match (paused job, malformed
    /// schedule that survived initial validation, etc.).
    next_fire_at: Option<i64>,
    /// Persisted run history. `None` when the job has never fired.
    run: Option<ScheduledRun>,
}

pub async fn handle(State(state): State<AppState>) -> Json<serde_json::Value> {
    let jobs = state.scheduler_jobs.as_ref();
    let mut entries: Vec<JobStatus<'_>> = Vec::with_capacity(jobs.len());
    for job in jobs.iter() {
        let next_fire_at = next_fire_for(job);
        let run = match state.scheduler_store.as_ref() {
            Some(store) => store.get(&job.id).await.ok().flatten(),
            None => None,
        };
        entries.push(JobStatus {
            id: &job.id,
            name: &job.name,
            enabled: job.enabled,
            cron: &job.cron,
            timezone: &job.timezone,
            platform: &job.platform,
            lane: &job.lane,
            next_fire_at,
            run,
        });
    }
    // Stable order matches `SchedulerStore::list` so client tooling
    // can join on id without sorting both sides.
    entries.sort_by(|a, b| a.id.cmp(b.id));
    Json(serde_json::json!({ "jobs": entries }))
}

/// Compute the next firing time for a job. Returns `None` when the
/// cron expression has no upcoming match within `cron`'s scan
/// horizon. Bad cron / timezone strings are silently treated as
/// "no upcoming fire" because they should have been rejected at
/// `SchedulerConfig::validate` time; the route is observability-
/// only and shouldn't 500 on data its inputs already vetted.
fn next_fire_for(job: &SchedulerJobConfig) -> Option<i64> {
    if !job.enabled {
        return None;
    }
    let schedule = cron::Schedule::from_str(&job.cron).ok()?;
    let tz = chrono_tz::Tz::from_str(&job.timezone).ok()?;
    let upcoming = schedule.upcoming(tz).next()?;
    Some(upcoming.with_timezone(&Utc).timestamp())
}
