//! Core Symphony poll-loop orchestration with fake runner boundaries.

use std::collections::{BTreeMap, BTreeSet};

use crate::symphony::config::PollingConfig;
use crate::symphony::task_source::TaskSource;
use crate::symphony::workflow::NormalizedTask;

pub const STALL_TIMEOUT_MS: u64 = 900_000;

#[derive(Debug, Clone)]
pub struct Orchestrator<S, R> {
    config: PollingConfig,
    source: S,
    runner: R,
    state: OrchestratorState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrchestratorState {
    pub running: BTreeMap<String, RunningTask>,
    pub claimed: BTreeSet<String>,
    pub retrying: BTreeMap<String, RetryPlan>,
    pub completed: BTreeSet<String>,
    pub token_totals: TokenTotals,
    pub rate_limits: BTreeMap<String, RateLimitSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningTask {
    pub task: NormalizedTask,
    pub run_id: String,
    pub started_at_ms: u64,
    pub last_seen_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPlan {
    pub next_at_ms: u64,
    pub attempt: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenTotals {
    pub input: u64,
    pub output: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitSnapshot {
    pub limit: u64,
    pub remaining: u64,
    pub reset_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollOutcome {
    pub events: Vec<OrchestratorEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrchestratorEvent {
    Reconciled,
    ConfigValidated,
    CandidatesFetched {
        count: usize,
    },
    Dispatched {
        id: String,
        run_id: String,
    },
    Completed {
        id: String,
    },
    Released {
        id: String,
        reason: ReleaseReason,
    },
    RefreshFailed,
    Requeued {
        id: String,
        next_at_ms: u64,
        reason: RetryReason,
    },
    StatusPublished,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseReason {
    Absent,
    NonActive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryReason {
    SlotUnavailable,
    Continuation,
    Failure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunResult {
    Succeeded {
        input_tokens: u64,
        output_tokens: u64,
        rate_limits: Vec<(String, RateLimitSnapshot)>,
    },
    NeedsContinuation,
    Failed,
}

pub trait TaskRunner {
    fn start(&mut self, task: &NormalizedTask, now_ms: u64) -> RunnerStartResult;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerStart {
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerStartResult {
    Started(RunnerStart),
    SlotUnavailable,
}

impl<S, R> Orchestrator<S, R>
where
    S: TaskSource,
    R: TaskRunner,
{
    pub fn new(config: PollingConfig, source: S, runner: R) -> Self {
        Self {
            config,
            source,
            runner,
            state: OrchestratorState::default(),
        }
    }

    pub fn state(&self) -> &OrchestratorState {
        &self.state
    }

    pub fn poll_tick(&mut self, now_ms: u64) -> PollOutcome {
        let mut events = self.reconcile(now_ms);
        events.push(OrchestratorEvent::Reconciled);
        events.push(OrchestratorEvent::ConfigValidated);
        let mut candidates = self
            .source
            .fetch_candidates(&self.config.active_states)
            .unwrap_or_default();
        candidates.sort_by(|left, right| left.identifier.cmp(&right.identifier));
        events.push(OrchestratorEvent::CandidatesFetched {
            count: candidates.len(),
        });

        for task in candidates {
            if self.state.running.len() >= self.config.max_concurrent {
                break;
            }
            if !self.is_eligible(&task, now_ms) {
                continue;
            }
            let start = self.runner.start(&task, now_ms);
            let id = task.id.clone();
            let RunnerStartResult::Started(start) = start else {
                let plan = self.schedule_retry(&id, now_ms, RetryReason::SlotUnavailable);
                events.push(OrchestratorEvent::Requeued {
                    id,
                    next_at_ms: plan.next_at_ms,
                    reason: RetryReason::SlotUnavailable,
                });
                continue;
            };
            self.state.claimed.insert(task.id.clone());
            self.state.running.insert(
                task.id.clone(),
                RunningTask {
                    task,
                    run_id: start.run_id.clone(),
                    started_at_ms: now_ms,
                    last_seen_ms: now_ms,
                },
            );
            events.push(OrchestratorEvent::Dispatched {
                id,
                run_id: start.run_id,
            });
        }

        events.push(OrchestratorEvent::StatusPublished);
        PollOutcome { events }
    }

    fn reconcile(&mut self, now_ms: u64) -> Vec<OrchestratorEvent> {
        if self.state.running.is_empty() {
            return Vec::new();
        }
        let ids = self.state.running.keys().cloned().collect::<Vec<_>>();
        let snapshots = match self.source.refresh_by_ids(&ids) {
            Ok(snapshots) => snapshots,
            Err(_) => return vec![OrchestratorEvent::RefreshFailed],
        };
        let snapshots = snapshots
            .into_iter()
            .map(|task| (task.id.clone(), task))
            .collect::<BTreeMap<_, _>>();
        let mut events = Vec::new();

        for id in ids {
            let Some(snapshot) = snapshots.get(&id).cloned() else {
                self.state.running.remove(&id);
                self.state.claimed.remove(&id);
                events.push(OrchestratorEvent::Released {
                    id,
                    reason: ReleaseReason::Absent,
                });
                continue;
            };

            if self.config.terminal_states.contains(&snapshot.state) {
                self.state.running.remove(&id);
                self.state.claimed.remove(&id);
                self.state.completed.insert(id.clone());
                events.push(OrchestratorEvent::Completed { id });
            } else if !self.config.active_states.contains(&snapshot.state) {
                self.state.running.remove(&id);
                self.state.claimed.remove(&id);
                events.push(OrchestratorEvent::Released {
                    id,
                    reason: ReleaseReason::NonActive,
                });
            } else if let Some(running) = self.state.running.get_mut(&id) {
                if now_ms.saturating_sub(running.last_seen_ms) > STALL_TIMEOUT_MS {
                    self.state.running.remove(&id);
                    self.state.claimed.remove(&id);
                    let plan = self.schedule_retry(&id, now_ms, RetryReason::Failure);
                    events.push(OrchestratorEvent::Requeued {
                        id,
                        next_at_ms: plan.next_at_ms,
                        reason: RetryReason::Failure,
                    });
                    continue;
                }
                running.task = snapshot;
                running.last_seen_ms = now_ms;
            }
        }

        events
    }

    pub fn record_run_result(&mut self, id: &str, result: RunResult, now_ms: u64) {
        match result {
            RunResult::Succeeded {
                input_tokens,
                output_tokens,
                rate_limits,
            } => {
                self.state.running.remove(id);
                self.state.claimed.remove(id);
                self.state.retrying.remove(id);
                self.state.completed.insert(id.to_string());
                self.state.token_totals.input += input_tokens;
                self.state.token_totals.output += output_tokens;
                for (name, snapshot) in rate_limits {
                    self.state.rate_limits.insert(name, snapshot);
                }
            }
            RunResult::NeedsContinuation => {
                self.schedule_retry(id, now_ms, RetryReason::Continuation);
            }
            RunResult::Failed => {
                self.schedule_retry(id, now_ms, RetryReason::Failure);
            }
        }
    }

    fn is_eligible(&self, task: &NormalizedTask, now_ms: u64) -> bool {
        !task.id.is_empty()
            && !task.identifier.is_empty()
            && !task.title.is_empty()
            && self.config.active_states.contains(&task.state)
            && !self.config.terminal_states.contains(&task.state)
            && !self.state.claimed.contains(&task.id)
            && !self.state.running.contains_key(&task.id)
            && self.retry_due(&task.id, now_ms)
            && self.state_slot_available(&task.state)
            && !task
                .blockers
                .iter()
                .any(|blocker| !self.state.completed.contains(blocker))
    }

    fn state_slot_available(&self, state: &str) -> bool {
        let Some(limit) = self.config.state_concurrency.get(state) else {
            return true;
        };
        let running = self
            .state
            .running
            .values()
            .filter(|running| running.task.state == state)
            .count();
        running < *limit
    }

    fn retry_due(&self, id: &str, now_ms: u64) -> bool {
        self.state
            .retrying
            .get(id)
            .is_none_or(|plan| plan.next_at_ms <= now_ms)
    }

    fn schedule_retry(&mut self, id: &str, now_ms: u64, reason: RetryReason) -> RetryPlan {
        let current_attempt = self.state.retrying.get(id).map_or(0, |plan| plan.attempt);
        let attempt = match reason {
            RetryReason::Continuation => current_attempt.max(1),
            RetryReason::SlotUnavailable | RetryReason::Failure => current_attempt + 1,
        };
        let delay_ms = match reason {
            RetryReason::Continuation => 250,
            RetryReason::SlotUnavailable => 1_000,
            RetryReason::Failure => 1_000 * 2u64.saturating_pow(attempt.saturating_sub(1)),
        };
        let plan = RetryPlan {
            next_at_ms: now_ms + delay_ms,
            attempt,
        };
        self.state.retrying.insert(id.to_string(), plan.clone());
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symphony::task_source::TaskSourceError;
    use serde_json::Value as JsonValue;

    #[test]
    fn poll_tick_reconciles_then_dispatches_eligible_candidates_until_slots_fill() {
        let source = FakeSource::new(vec![
            task("1", "TASK-1", "ready-for-agent"),
            task("2", "TASK-2", "ready-for-agent"),
            task("3", "TASK-3", "blocked"),
        ]);
        let runner = FakeRunner::default();
        let mut orchestrator = Orchestrator::new(config(2), source, runner);

        let outcome = orchestrator.poll_tick(100);

        assert_eq!(
            outcome.events,
            [
                OrchestratorEvent::Reconciled,
                OrchestratorEvent::ConfigValidated,
                OrchestratorEvent::CandidatesFetched { count: 2 },
                OrchestratorEvent::Dispatched {
                    id: "1".into(),
                    run_id: "run-1".into()
                },
                OrchestratorEvent::Dispatched {
                    id: "2".into(),
                    run_id: "run-2".into()
                },
                OrchestratorEvent::StatusPublished,
            ]
        );
        assert_eq!(orchestrator.state().running.len(), 2);
        assert_eq!(orchestrator.state().claimed.len(), 2);
    }

    #[test]
    fn candidate_eligibility_enforces_required_fields_claims_blockers_and_state_slots() {
        let duplicate = task("already", "TASK-ALREADY", "ready-for-agent");
        let mut blocked = task("blocked", "TASK-BLOCKED", "ready-for-agent");
        blocked.blockers = vec!["TASK-MISSING".into()];
        let mut missing_title = task("missing-title", "TASK-MISSING-TITLE", "ready-for-agent");
        missing_title.title.clear();

        let source = FakeSource::new(vec![
            duplicate.clone(),
            task("allowed", "TASK-ALLOWED", "ready-for-agent"),
            task("same-state-over-cap", "TASK-OVER-CAP", "ready-for-agent"),
            blocked,
            missing_title,
        ]);
        let mut cfg = config(10);
        cfg.state_concurrency.insert("ready-for-agent".into(), 2);
        let mut orchestrator = Orchestrator::new(cfg, source, FakeRunner::default());
        orchestrator.state.claimed.insert("already".into());
        let mut running_duplicate = duplicate;
        running_duplicate.state = "running".into();
        orchestrator.state.running.insert(
            "already".into(),
            RunningTask {
                task: running_duplicate,
                run_id: "existing-run".into(),
                started_at_ms: 0,
                last_seen_ms: 0,
            },
        );

        orchestrator.poll_tick(100);

        assert_eq!(
            orchestrator
                .state()
                .running
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["allowed", "already"]
        );
        assert!(!orchestrator.state().claimed.contains("blocked"));
        assert!(!orchestrator.state().claimed.contains("missing-title"));
        assert!(!orchestrator.state().claimed.contains("same-state-over-cap"));
    }

    #[test]
    fn retry_scheduling_handles_slot_unavailable_continuation_and_exponential_backoff() {
        let source = FakeSource::new(vec![task("1", "TASK-1", "ready-for-agent")]);
        let mut runner = FakeRunner::default();
        runner.slot_unavailable = true;
        let mut orchestrator = Orchestrator::new(config(1), source, runner);

        let outcome = orchestrator.poll_tick(1_000);
        assert!(outcome.events.contains(&OrchestratorEvent::Requeued {
            id: "1".into(),
            next_at_ms: 2_000,
            reason: RetryReason::SlotUnavailable,
        }));
        assert_eq!(
            orchestrator.state().retrying.get("1"),
            Some(&RetryPlan {
                next_at_ms: 2_000,
                attempt: 1,
            })
        );

        orchestrator.record_run_result("1", RunResult::NeedsContinuation, 2_000);
        assert_eq!(
            orchestrator.state().retrying.get("1"),
            Some(&RetryPlan {
                next_at_ms: 2_250,
                attempt: 1,
            })
        );

        orchestrator.record_run_result("1", RunResult::Failed, 3_000);
        assert_eq!(
            orchestrator.state().retrying.get("1"),
            Some(&RetryPlan {
                next_at_ms: 5_000,
                attempt: 2,
            })
        );
    }

    #[test]
    fn reconciliation_completes_terminal_releases_absent_and_non_active_and_refreshes_active() {
        let source = FakeSource::new(vec![
            task("keep", "TASK-KEEP", "ready-for-agent"),
            task("terminal", "TASK-TERMINAL", "closed"),
            task("paused", "TASK-PAUSED", "triage"),
        ]);
        let mut orchestrator = Orchestrator::new(config(10), source, FakeRunner::default());
        for id in ["keep", "terminal", "absent", "paused"] {
            let running = RunningTask {
                task: task(
                    id,
                    &format!("TASK-{}", id.to_uppercase()),
                    "ready-for-agent",
                ),
                run_id: format!("run-{id}"),
                started_at_ms: 0,
                last_seen_ms: 0,
            };
            orchestrator.state.claimed.insert(id.into());
            orchestrator.state.running.insert(id.into(), running);
        }

        let outcome = orchestrator.poll_tick(1_000);

        assert!(outcome.events.contains(&OrchestratorEvent::Completed {
            id: "terminal".into()
        }));
        assert!(outcome.events.contains(&OrchestratorEvent::Released {
            id: "absent".into(),
            reason: ReleaseReason::Absent,
        }));
        assert!(outcome.events.contains(&OrchestratorEvent::Released {
            id: "paused".into(),
            reason: ReleaseReason::NonActive,
        }));
        assert_eq!(
            orchestrator.state().running.keys().collect::<Vec<_>>(),
            vec![&"keep".to_string()]
        );
        assert_eq!(orchestrator.state().running["keep"].last_seen_ms, 1_000);
        assert!(orchestrator.state().completed.contains("terminal"));
    }

    #[test]
    fn reconciliation_keeps_state_on_refresh_failure_and_retries_stalled_runs() {
        let mut failing_source =
            FakeSource::new(vec![task("keep", "TASK-KEEP", "ready-for-agent")]);
        failing_source.refresh_error = true;
        let mut orchestrator = Orchestrator::new(config(10), failing_source, FakeRunner::default());
        orchestrator.insert_running("keep", 0, "ready-for-agent");

        let outcome = orchestrator.poll_tick(1_000);
        assert!(outcome.events.contains(&OrchestratorEvent::RefreshFailed));
        assert!(orchestrator.state().running.contains_key("keep"));

        let source = FakeSource::new(vec![task("stale", "TASK-STALE", "ready-for-agent")]);
        let mut orchestrator = Orchestrator::new(config(10), source, FakeRunner::default());
        orchestrator.insert_running("stale", 0, "ready-for-agent");

        let outcome = orchestrator.poll_tick(STALL_TIMEOUT_MS + 1);
        assert!(outcome.events.contains(&OrchestratorEvent::Requeued {
            id: "stale".into(),
            next_at_ms: STALL_TIMEOUT_MS + 1_001,
            reason: RetryReason::Failure,
        }));
        assert!(!orchestrator.state().running.contains_key("stale"));
        assert!(!orchestrator.state().claimed.contains("stale"));
    }

    #[test]
    fn successful_runs_update_completed_bookkeeping_tokens_and_rate_limits() {
        let source = FakeSource::new(Vec::new());
        let mut orchestrator = Orchestrator::new(config(10), source, FakeRunner::default());
        orchestrator.insert_running("done", 0, "ready-for-agent");

        orchestrator.record_run_result(
            "done",
            RunResult::Succeeded {
                input_tokens: 42,
                output_tokens: 7,
                rate_limits: vec![(
                    "requests".into(),
                    RateLimitSnapshot {
                        limit: 100,
                        remaining: 93,
                        reset_at_ms: Some(60_000),
                    },
                )],
            },
            1_000,
        );

        assert!(orchestrator.state().completed.contains("done"));
        assert!(!orchestrator.state().running.contains_key("done"));
        assert_eq!(
            orchestrator.state().token_totals,
            TokenTotals {
                input: 42,
                output: 7,
            }
        );
        assert_eq!(
            orchestrator.state().rate_limits["requests"],
            RateLimitSnapshot {
                limit: 100,
                remaining: 93,
                reset_at_ms: Some(60_000),
            }
        );
    }

    #[derive(Debug, Clone)]
    struct FakeSource {
        tasks: Vec<NormalizedTask>,
        refresh_error: bool,
    }

    impl FakeSource {
        fn new(tasks: Vec<NormalizedTask>) -> Self {
            Self {
                tasks,
                refresh_error: false,
            }
        }
    }

    impl TaskSource for FakeSource {
        fn capabilities(&self) -> crate::symphony::task_source::TaskSourceCapabilities {
            crate::symphony::task_source::TaskSourceCapabilities {
                fetch_candidates: true,
                fetch_by_state: true,
                refresh_by_ids: true,
            }
        }

        fn fetch_candidates(
            &self,
            active_states: &[String],
        ) -> Result<Vec<NormalizedTask>, TaskSourceError> {
            Ok(self
                .tasks
                .iter()
                .filter(|task| active_states.contains(&task.state))
                .cloned()
                .collect())
        }

        fn fetch_by_state(
            &self,
            states: &[String],
        ) -> Result<Vec<NormalizedTask>, TaskSourceError> {
            Ok(self
                .tasks
                .iter()
                .filter(|task| states.contains(&task.state))
                .cloned()
                .collect())
        }

        fn refresh_by_ids(&self, ids: &[String]) -> Result<Vec<NormalizedTask>, TaskSourceError> {
            if self.refresh_error {
                return Err(TaskSourceError::UnsupportedKind {
                    kind: "test refresh error".into(),
                });
            }
            Ok(self
                .tasks
                .iter()
                .filter(|task| ids.contains(&task.id))
                .cloned()
                .collect())
        }
    }

    impl Orchestrator<FakeSource, FakeRunner> {
        fn insert_running(&mut self, id: &str, last_seen_ms: u64, state: &str) {
            self.state.claimed.insert(id.into());
            self.state.running.insert(
                id.into(),
                RunningTask {
                    task: task(id, &format!("TASK-{}", id.to_uppercase()), state),
                    run_id: format!("run-{id}"),
                    started_at_ms: 0,
                    last_seen_ms,
                },
            );
        }
    }

    #[derive(Debug, Clone, Default)]
    struct FakeRunner {
        next: usize,
        slot_unavailable: bool,
    }

    impl TaskRunner for FakeRunner {
        fn start(&mut self, _task: &NormalizedTask, _now_ms: u64) -> RunnerStartResult {
            if self.slot_unavailable {
                return RunnerStartResult::SlotUnavailable;
            }
            self.next += 1;
            RunnerStartResult::Started(RunnerStart {
                run_id: format!("run-{}", self.next),
            })
        }
    }

    fn config(max_concurrent: usize) -> PollingConfig {
        PollingConfig {
            interval_secs: 60,
            active_states: vec!["ready-for-agent".into()],
            terminal_states: vec!["closed".into(), "done".into()],
            max_concurrent,
            state_concurrency: BTreeMap::new(),
        }
    }

    fn task(id: &str, identifier: &str, state: &str) -> NormalizedTask {
        NormalizedTask {
            id: id.into(),
            identifier: identifier.into(),
            title: format!("{identifier} title"),
            body: String::new(),
            priority: None,
            state: state.into(),
            branch: None,
            labels: Vec::new(),
            blockers: Vec::new(),
            url: None,
            path: None,
            created_at: None,
            updated_at: None,
            source: BTreeMap::<String, JsonValue>::new(),
        }
    }
}
