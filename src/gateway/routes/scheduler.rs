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
        let run = state
            .scheduler_store
            .as_ref()
            .and_then(|store| store.get(&job.id).ok().flatten());
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::config::{OverlapPolicy, SchedulerJobConfig};
    use crate::gateway::lane_router::DaemonLaneRouter;
    use crate::gateway::queue::{InboundQueue, OutboundQueue};
    use crate::gateway::registry::PlatformRegistry;
    use crate::gateway::scheduler_store::SchedulerStore;
    use crate::gateway::server::{AppState, build_router};
    use crate::memory::DbPool;

    fn fresh_db() -> DbPool {
        crate::memory::in_memory_gateway_pool().expect("in-memory pool")
    }

    fn job(id: &str, cron: &str) -> SchedulerJobConfig {
        SchedulerJobConfig {
            id: id.into(),
            name: format!("{id}-name"),
            enabled: true,
            cron: cron.into(),
            timezone: "UTC".into(),
            platform: "loopback".into(),
            lane: "c1".into(),
            prompt: "do thing".into(),
            max_runtime_secs: None,
            overlap_policy: OverlapPolicy::Skip,
        }
    }

    fn build_app_state(
        db: DbPool,
        jobs: Vec<SchedulerJobConfig>,
        store: Option<SchedulerStore>,
    ) -> AppState {
        let lane_router = Arc::new(DaemonLaneRouter::with_client_factory(|| {
            Box::pin(async {
                Err(crate::client::ClientError::Protocol(
                    "scheduler route test: client factory must not be invoked".into(),
                ))
            })
        }));
        AppState {
            api_token: Arc::new("secret".into()),
            inbound: Arc::new(InboundQueue::new(db.clone())),
            outbound: Arc::new(OutboundQueue::new(db.clone(), 5)),
            registry: Arc::new(PlatformRegistry::new()),
            lane_router,
            scheduler_jobs: Arc::new(jobs),
            scheduler_store: store,
        }
    }

    // YYC-17 PR-4: empty config returns `{"jobs": []}`.
    #[tokio::test]
    async fn get_scheduler_returns_empty_when_no_jobs() {
        let db = fresh_db();
        let state = build_app_state(db, vec![], None);
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/scheduler")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["jobs"], serde_json::json!([]));
    }

    // YYC-17 PR-4: configured jobs without run history surface with
    // `run = null` and a populated `next_fire_at`.
    #[tokio::test]
    async fn get_scheduler_lists_configured_jobs_without_runs() {
        let db = fresh_db();
        let store = SchedulerStore::new(db.clone());
        let jobs = vec![job("daily", "0 8 * * * *")];
        let state = build_app_state(db, jobs, Some(store));
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/scheduler")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let jobs_array = json["jobs"].as_array().expect("jobs array");
        assert_eq!(jobs_array.len(), 1);
        assert_eq!(jobs_array[0]["id"], "daily");
        assert_eq!(jobs_array[0]["enabled"], true);
        assert!(jobs_array[0]["next_fire_at"].is_number());
        assert!(jobs_array[0]["run"].is_null());
    }

    // YYC-17 PR-4: persisted run history shows up under each job's
    // `run` field and counts merge correctly.
    #[tokio::test]
    async fn get_scheduler_merges_run_history() {
        let db = fresh_db();
        let store = SchedulerStore::new(db.clone());
        store.record_enqueued("daily", 1_700_000_000, 42).unwrap();
        store.record_skipped("daily", 1_700_000_100).unwrap();
        let jobs = vec![job("daily", "0 8 * * * *")];
        let state = build_app_state(db, jobs, Some(store));
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/scheduler")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let entry = &json["jobs"][0];
        let run = &entry["run"];
        assert_eq!(run["last_status"], "skipped");
        assert_eq!(run["total_fires"], 2);
        assert_eq!(run["skipped_fires"], 1);
        assert_eq!(run["last_inbound_id"], 42);
    }

    // YYC-17 PR-4: disabled jobs surface with `next_fire_at = null`.
    #[tokio::test]
    async fn get_scheduler_disabled_jobs_have_no_next_fire() {
        let db = fresh_db();
        let mut j = job("paused", "0 8 * * * *");
        j.enabled = false;
        let state = build_app_state(db, vec![j], None);
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/scheduler")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["jobs"][0]["enabled"], false);
        assert!(json["jobs"][0]["next_fire_at"].is_null());
    }

    // YYC-17 PR-4: bearer-auth gates the route.
    #[tokio::test]
    async fn get_scheduler_requires_bearer() {
        let db = fresh_db();
        let state = build_app_state(db, vec![], None);
        let app = build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/v1/scheduler")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
