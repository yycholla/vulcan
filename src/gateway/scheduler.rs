//! YYC-17 PR-2: cron scheduler that enqueues firings into the
//! gateway inbound queue.
//!
//! Each scheduled job's prompt is delivered as a synthetic
//! [`InboundMessage`] keyed by the configured `platform` + `lane`,
//! so the existing gateway worker pipeline (lane router, agent map,
//! outbound delivery) handles the run with no special-case code.
//! The scheduler itself is a single tokio task that wakes at the
//! next firing time across all enabled jobs.
//!
//! # Scope of this PR
//!
//! - Parse + validate config (lives in `crate::config::SchedulerConfig`).
//! - Build a [`Scheduler`] from config and spawn the firing loop.
//! - Enqueue an `InboundMessage` per firing.
//! - Tracing on fire / enqueue failure with job id + name.
//!
//! # Deliberately deferred (follow-up PRs)
//!
//! - Persistent next-run / last-run state in SQLite (PR-C-3).
//! - Real overlap-policy enforcement (`Skip` / `Replace` need a
//!   durable view of in-flight runs that PR-C-3 establishes).
//! - Admin endpoint exposing schedule status.

use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::task::JoinHandle;

use crate::config::{SchedulerConfig, SchedulerJobConfig};
use crate::gateway::queue::InboundQueue;
use crate::platform::InboundMessage;

/// Synthetic `user_id` stamped on scheduler firings so downstream
/// code (audit, observability) can distinguish them from real
/// platform users without inventing extra columns.
pub const SCHEDULER_USER_ID: &str = "scheduler";

pub struct Scheduler {
    jobs: Vec<RunningJob>,
    inbound: Arc<InboundQueue>,
}

#[derive(Clone)]
struct RunningJob {
    config: SchedulerJobConfig,
    schedule: cron::Schedule,
    tz: chrono_tz::Tz,
}

pub struct SchedulerHandle {
    handle: JoinHandle<()>,
}

impl Drop for SchedulerHandle {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl Scheduler {
    /// Build a scheduler from the parsed config. Validation runs up
    /// front so a bad cron expression or unknown timezone surfaces
    /// before we spawn anything. Disabled jobs are filtered here so
    /// the runtime loop only ever sees ones it should fire.
    pub fn from_config(config: &SchedulerConfig, inbound: Arc<InboundQueue>) -> Result<Self> {
        config.validate()?;
        let mut jobs = Vec::new();
        for job in config.jobs.iter().filter(|j| j.enabled) {
            let schedule = cron::Schedule::from_str(&job.cron)
                .with_context(|| format!("scheduler job '{}': cron parse", job.id))?;
            let tz = chrono_tz::Tz::from_str(&job.timezone).map_err(|e| {
                anyhow::anyhow!("scheduler job '{}': invalid timezone: {e}", job.id)
            })?;
            jobs.push(RunningJob {
                config: job.clone(),
                schedule,
                tz,
            });
        }
        Ok(Self { jobs, inbound })
    }

    /// Number of enabled jobs. Used by the gateway runtime to skip
    /// spawning the loop entirely when nothing's configured.
    pub fn enabled_jobs(&self) -> usize {
        self.jobs.len()
    }

    /// Spawn the firing loop. The returned handle aborts the task
    /// when dropped so callers don't have to manually clean up on
    /// shutdown.
    pub fn spawn(self) -> SchedulerHandle {
        let handle = tokio::spawn(async move {
            self.run().await;
        });
        SchedulerHandle { handle }
    }

    async fn run(mut self) {
        if self.jobs.is_empty() {
            tracing::info!(
                target: "gateway::scheduler",
                "no enabled jobs; scheduler loop exiting",
            );
            return;
        }
        tracing::info!(
            target: "gateway::scheduler",
            jobs = self.jobs.len(),
            "scheduler started",
        );
        loop {
            let now = Utc::now();
            // Find the soonest upcoming fire across all jobs. cron
            // returns an iterator in the job's timezone; convert to
            // UTC for the wait calculation.
            let mut soonest: Option<(usize, chrono::DateTime<Utc>)> = None;
            for (i, job) in self.jobs.iter().enumerate() {
                if let Some(next_in_tz) = job.schedule.upcoming(job.tz).next() {
                    let next_utc = next_in_tz.with_timezone(&Utc);
                    if soonest.is_none_or(|(_, s)| next_utc < s) {
                        soonest = Some((i, next_utc));
                    }
                }
            }
            let Some((idx, fire_at)) = soonest else {
                tracing::warn!(
                    target: "gateway::scheduler",
                    "no upcoming fires across any job; loop exiting",
                );
                return;
            };
            let wait = (fire_at - now).to_std().unwrap_or_default();
            tokio::time::sleep(wait).await;
            self.fire(idx).await;
        }
    }

    async fn fire(&mut self, idx: usize) {
        let job = &self.jobs[idx];
        let msg = build_inbound_message_for_job(&job.config);
        match self.inbound.enqueue(msg).await {
            Ok(row_id) => {
                tracing::info!(
                    target: "gateway::scheduler",
                    job_id = %job.config.id,
                    job_name = %job.config.name,
                    platform = %job.config.platform,
                    lane = %job.config.lane,
                    row_id,
                    "scheduler firing enqueued",
                );
            }
            Err(e) => {
                tracing::error!(
                    target: "gateway::scheduler",
                    job_id = %job.config.id,
                    error = %e,
                    "scheduler enqueue failed",
                );
            }
        }
    }
}

/// Build the synthetic `InboundMessage` a scheduler firing produces.
/// Pure function so unit tests don't need a live tokio runtime or
/// a database to validate the wire shape.
pub fn build_inbound_message_for_job(job: &SchedulerJobConfig) -> InboundMessage {
    InboundMessage {
        platform: job.platform.clone(),
        chat_id: job.lane.clone(),
        user_id: SCHEDULER_USER_ID.into(),
        text: job.prompt.clone(),
        message_id: None,
        reply_to: None,
        attachments: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OverlapPolicy;

    fn job(id: &str, cron: &str) -> SchedulerJobConfig {
        SchedulerJobConfig {
            id: id.into(),
            name: format!("name-{id}"),
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

    // YYC-17 PR-2: build_inbound_message_for_job carries the job's
    // platform / lane / prompt and stamps the synthetic user id so
    // worker code can distinguish scheduler firings.
    #[test]
    fn build_inbound_message_carries_job_fields() {
        let inbound = build_inbound_message_for_job(&job("daily", "0 8 * * * *"));
        assert_eq!(inbound.platform, "loopback");
        assert_eq!(inbound.chat_id, "c1");
        assert_eq!(inbound.user_id, SCHEDULER_USER_ID);
        assert_eq!(inbound.text, "do thing");
        assert!(inbound.attachments.is_empty());
        assert!(inbound.message_id.is_none());
    }

    // YYC-17 PR-2: from_config rejects bad cron at construction.
    #[tokio::test]
    async fn scheduler_from_config_rejects_bad_cron() {
        let pool = crate::memory::in_memory_gateway_pool().expect("pool");
        let inbound = Arc::new(InboundQueue::new(pool));
        let mut config = SchedulerConfig::default();
        config.jobs.push(job("bad", "not a cron"));
        let err = match Scheduler::from_config(&config, inbound) {
            Ok(_) => panic!("expected from_config to fail on bad cron"),
            Err(e) => e,
        };
        assert!(format!("{err:#}").contains("cron"));
    }

    // YYC-17 PR-2: disabled jobs don't get loaded into the runtime.
    #[tokio::test]
    async fn scheduler_skips_disabled_jobs() {
        let pool = crate::memory::in_memory_gateway_pool().expect("pool");
        let inbound = Arc::new(InboundQueue::new(pool));
        let mut config = SchedulerConfig::default();
        let mut j = job("ok", "0 8 * * * *");
        j.enabled = false;
        config.jobs.push(j);
        let scheduler = Scheduler::from_config(&config, inbound).expect("ok");
        assert_eq!(scheduler.enabled_jobs(), 0);
    }

    // YYC-17 PR-2: a configured + enabled job round-trips through
    // from_config and shows up in the runtime job list.
    #[tokio::test]
    async fn scheduler_loads_enabled_jobs() {
        let pool = crate::memory::in_memory_gateway_pool().expect("pool");
        let inbound = Arc::new(InboundQueue::new(pool));
        let mut config = SchedulerConfig::default();
        config.jobs.push(job("a", "0 8 * * * *"));
        config.jobs.push(job("b", "0 9 * * * *"));
        let scheduler = Scheduler::from_config(&config, inbound).expect("ok");
        assert_eq!(scheduler.enabled_jobs(), 2);
    }
}
