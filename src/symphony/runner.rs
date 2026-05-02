//! Symphony agent-runner integration over workspace, workflow, and app-server boundaries.

use crate::symphony::app_server::{
    AppServerClient, AppServerError, AppServerOutcome, AppServerRequest,
};
use crate::symphony::config::AgentConfig;
use crate::symphony::orchestrator::{RateLimitSnapshot, RunResult};
use crate::symphony::workflow::{NormalizedTask, PromptInput, Workflow, WorkflowError};
use crate::symphony::workspace::WorkspaceManager;

const CONTINUATION_GUIDANCE: &str = "Continue working on the existing task thread. Do not resend \
the original task prompt. Use the current workspace state and provide the next terminal outcome.";

#[derive(Debug, Clone)]
pub struct AgentRunner<W> {
    workflow: Workflow,
    workspace: WorkspaceManager,
    config: AgentConfig,
    worker: W,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRunRequest {
    pub task: NormalizedTask,
    pub attempt: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRunReport {
    pub result: RunResult,
    pub terminal_reason: AgentTerminalReason,
    pub events: Vec<AgentRunnerEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentTerminalReason {
    Succeeded,
    WorkerFailed,
    WorkerCancelled,
    WorkerTimedOut,
    WorkerExited,
    MalformedMessage,
    UnsupportedToolCall,
    InputRequired,
    MaxTurns,
    WorkspaceInvalid,
    PromptRenderFailed,
    WorkerLaunchFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRunnerEvent {
    WorkspacePrepared {
        path: String,
    },
    TurnStarted {
        turn: u32,
        prompt_kind: PromptKind,
    },
    TurnCompleted {
        turn: u32,
    },
    TurnTerminal {
        turn: u32,
        reason: AgentTerminalReason,
    },
    ContinuationRequested {
        turn: u32,
    },
    WorkerCleanedUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Initial,
    Continuation,
}

pub trait AgentWorker {
    type Error;

    fn run_turn(&mut self, request: AppServerRequest) -> Result<AppServerOutcome, Self::Error>;
    fn cleanup(&mut self);
}

impl AgentWorker for AppServerClient {
    type Error = AppServerError;

    fn run_turn(&mut self, request: AppServerRequest) -> Result<AppServerOutcome, Self::Error> {
        AppServerClient::run_turn(self, request)
    }

    fn cleanup(&mut self) {}
}

impl<W> AgentRunner<W>
where
    W: AgentWorker,
{
    pub fn new(
        workflow: Workflow,
        workspace: WorkspaceManager,
        config: AgentConfig,
        worker: W,
    ) -> Self {
        Self {
            workflow,
            workspace,
            config,
            worker,
        }
    }

    pub fn worker(&self) -> &W {
        &self.worker
    }

    pub fn run_attempt(&mut self, request: AgentRunRequest) -> AgentRunReport {
        let mut events = Vec::new();
        let prepared = match self.workspace.prepare(&request.task) {
            Ok(prepared) => prepared,
            Err(_err) => return terminal(RunResult::Failed, AgentTerminalReason::WorkspaceInvalid),
        };
        let workspace_path = prepared.path.display().to_string();
        events.push(AgentRunnerEvent::WorkspacePrepared {
            path: workspace_path.clone(),
        });

        if self.workspace.before_run(&prepared).is_err() {
            return terminal_with_cleanup(
                RunResult::Failed,
                AgentTerminalReason::WorkspaceInvalid,
                events,
                &mut self.worker,
                &self.workspace,
                &prepared,
            );
        }

        let mut prompt = match render_initial_prompt(&self.workflow, &request.task, request.attempt)
        {
            Ok(prompt) => prompt,
            Err(_err) => {
                return terminal_with_cleanup(
                    RunResult::Failed,
                    AgentTerminalReason::PromptRenderFailed,
                    events,
                    &mut self.worker,
                    &self.workspace,
                    &prepared,
                );
            }
        };
        let task = request.task;
        let max_turns = self.config.max_turns.max(1);

        for turn in 1..=max_turns {
            let prompt_kind = if turn == 1 {
                PromptKind::Initial
            } else {
                PromptKind::Continuation
            };
            events.push(AgentRunnerEvent::TurnStarted { turn, prompt_kind });
            let outcome = self.worker.run_turn(AppServerRequest {
                task: task.clone(),
                workspace: prepared.path.clone(),
                prompt,
                attempt: request.attempt,
            });

            match outcome {
                Ok(AppServerOutcome::Completed(telemetry)) => {
                    events.push(AgentRunnerEvent::TurnCompleted { turn });
                    return terminal_with_cleanup(
                        RunResult::Succeeded {
                            input_tokens: telemetry.input_tokens,
                            output_tokens: telemetry.output_tokens,
                            rate_limits: telemetry
                                .rate_limits
                                .into_iter()
                                .map(|limit| {
                                    (
                                        limit.name,
                                        RateLimitSnapshot {
                                            limit: limit.limit,
                                            remaining: limit.remaining,
                                            reset_at_ms: limit.reset_at_ms,
                                        },
                                    )
                                })
                                .collect(),
                        },
                        AgentTerminalReason::Succeeded,
                        events,
                        &mut self.worker,
                        &self.workspace,
                        &prepared,
                    );
                }
                Ok(AppServerOutcome::InputRequired { prompt: next }) if turn < max_turns => {
                    events.push(AgentRunnerEvent::ContinuationRequested { turn });
                    prompt = continuation_prompt(next.as_deref());
                    continue;
                }
                Ok(AppServerOutcome::InputRequired { .. }) => {
                    events.push(AgentRunnerEvent::TurnTerminal {
                        turn,
                        reason: AgentTerminalReason::MaxTurns,
                    });
                    return terminal_with_cleanup(
                        RunResult::NeedsContinuation,
                        AgentTerminalReason::MaxTurns,
                        events,
                        &mut self.worker,
                        &self.workspace,
                        &prepared,
                    );
                }
                other => {
                    let (result, reason) = map_non_success_outcome(other);
                    events.push(AgentRunnerEvent::TurnTerminal {
                        turn,
                        reason: reason.clone(),
                    });
                    return terminal_with_cleanup(
                        result,
                        reason,
                        events,
                        &mut self.worker,
                        &self.workspace,
                        &prepared,
                    );
                }
            }
        }

        terminal_with_cleanup(
            RunResult::NeedsContinuation,
            AgentTerminalReason::MaxTurns,
            events,
            &mut self.worker,
            &self.workspace,
            &prepared,
        )
    }
}

fn map_non_success_outcome<E>(
    outcome: Result<AppServerOutcome, E>,
) -> (RunResult, AgentTerminalReason) {
    match outcome {
        Ok(AppServerOutcome::Failed { .. }) => {
            (RunResult::Failed, AgentTerminalReason::WorkerFailed)
        }
        Ok(AppServerOutcome::Cancelled) => {
            (RunResult::Failed, AgentTerminalReason::WorkerCancelled)
        }
        Ok(AppServerOutcome::TimedOut) => (RunResult::Failed, AgentTerminalReason::WorkerTimedOut),
        Ok(AppServerOutcome::ProcessExited { .. }) => {
            (RunResult::Failed, AgentTerminalReason::WorkerExited)
        }
        Ok(AppServerOutcome::MalformedMessage { .. }) => {
            (RunResult::Failed, AgentTerminalReason::MalformedMessage)
        }
        Ok(AppServerOutcome::UnsupportedToolCall { .. }) => {
            (RunResult::Failed, AgentTerminalReason::UnsupportedToolCall)
        }
        Ok(AppServerOutcome::InputRequired { .. }) => (
            RunResult::NeedsContinuation,
            AgentTerminalReason::InputRequired,
        ),
        Ok(AppServerOutcome::Completed(_)) => unreachable!("completed is handled before mapping"),
        Err(_err) => (RunResult::Failed, AgentTerminalReason::WorkerLaunchFailed),
    }
}

fn continuation_prompt(worker_prompt: Option<&str>) -> String {
    match worker_prompt {
        Some(prompt) if !prompt.trim().is_empty() => {
            format!(
                "{CONTINUATION_GUIDANCE}\n\nWorker requested input:\n{}",
                prompt.trim()
            )
        }
        _ => CONTINUATION_GUIDANCE.to_string(),
    }
}

fn render_initial_prompt(
    workflow: &Workflow,
    task: &NormalizedTask,
    attempt: u32,
) -> Result<String, WorkflowError> {
    workflow.render_prompt(&PromptInput {
        issue: task.clone(),
        attempt: Some(attempt),
    })
}

fn terminal(result: RunResult, terminal_reason: AgentTerminalReason) -> AgentRunReport {
    AgentRunReport {
        result,
        terminal_reason,
        events: Vec::new(),
    }
}

fn terminal_with_cleanup<W>(
    result: RunResult,
    terminal_reason: AgentTerminalReason,
    mut events: Vec<AgentRunnerEvent>,
    worker: &mut W,
    workspace: &WorkspaceManager,
    prepared: &crate::symphony::workspace::PreparedWorkspace,
) -> AgentRunReport
where
    W: AgentWorker,
{
    worker.cleanup();
    events.push(AgentRunnerEvent::WorkerCleanedUp);
    workspace.after_run(prepared);
    AgentRunReport {
        result,
        terminal_reason,
        events,
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::symphony::app_server::{AppServerOutcome, AppServerRateLimit, AppServerTelemetry};
    use crate::symphony::config::{AgentConfig, WorkspaceConfig};
    use crate::symphony::orchestrator::{RateLimitSnapshot, RunResult};
    use crate::symphony::workflow::{NormalizedTask, Workflow};
    use crate::symphony::workspace::WorkspaceManager;

    #[test]
    fn first_attempt_renders_full_prompt_runs_worker_and_reports_success() {
        let temp = TempDir::new().unwrap();
        let workflow = Workflow::parse("Implement {{ issue.identifier }}: {{ issue.title }}")
            .expect("workflow");
        let workspace = WorkspaceManager::new(WorkspaceConfig {
            root: temp.path().join("workspaces"),
            preserve_success: true,
        });
        let worker = FakeWorker::new(vec![Ok(AppServerOutcome::Completed(AppServerTelemetry {
            session_id: "GH-601:session".into(),
            input_tokens: 40,
            output_tokens: 8,
            messages: Vec::new(),
            rate_limits: vec![AppServerRateLimit {
                name: "requests".into(),
                limit: 100,
                remaining: 91,
                reset_at_ms: Some(60_000),
            }],
        }))]);
        let mut runner = AgentRunner::new(
            workflow,
            workspace,
            AgentConfig {
                max_attempts: 3,
                max_turns: 2,
                stall_timeout_secs: Some(900),
            },
            worker,
        );

        let report = runner.run_attempt(AgentRunRequest {
            task: task("601", "GH-601"),
            attempt: 1,
        });

        assert_eq!(
            report.result,
            RunResult::Succeeded {
                input_tokens: 40,
                output_tokens: 8,
                rate_limits: vec![(
                    "requests".into(),
                    RateLimitSnapshot {
                        limit: 100,
                        remaining: 91,
                        reset_at_ms: Some(60_000),
                    },
                )],
            }
        );
        assert_eq!(report.terminal_reason, AgentTerminalReason::Succeeded);
        assert_eq!(runner.worker().prompts, ["Implement GH-601: Agent runner"]);
        assert!(runner.worker().workspaces[0].ends_with("GH-601"));
        assert_eq!(runner.worker().cleanup_calls, 1);
        assert_eq!(
            report.events,
            [
                AgentRunnerEvent::WorkspacePrepared {
                    path: runner.worker().workspaces[0].clone(),
                },
                AgentRunnerEvent::TurnStarted {
                    turn: 1,
                    prompt_kind: PromptKind::Initial,
                },
                AgentRunnerEvent::TurnCompleted { turn: 1 },
                AgentRunnerEvent::WorkerCleanedUp,
            ]
        );
    }

    #[test]
    fn input_required_continuation_reuses_live_worker_and_sends_guidance() {
        let temp = TempDir::new().unwrap();
        let workflow =
            Workflow::parse("Original {{ issue.identifier }} task prompt").expect("workflow");
        let workspace = WorkspaceManager::new(WorkspaceConfig {
            root: temp.path().join("workspaces"),
            preserve_success: true,
        });
        let worker = FakeWorker::new(vec![
            Ok(AppServerOutcome::InputRequired {
                prompt: Some("Need next instruction".into()),
            }),
            Ok(AppServerOutcome::Completed(AppServerTelemetry {
                session_id: "GH-601:session".into(),
                input_tokens: 5,
                output_tokens: 3,
                messages: Vec::new(),
                rate_limits: Vec::new(),
            })),
        ]);
        let mut runner = AgentRunner::new(
            workflow,
            workspace,
            AgentConfig {
                max_attempts: 3,
                max_turns: 2,
                stall_timeout_secs: Some(900),
            },
            worker,
        );

        let report = runner.run_attempt(AgentRunRequest {
            task: task("601", "GH-601"),
            attempt: 1,
        });

        assert_eq!(report.terminal_reason, AgentTerminalReason::Succeeded);
        assert_eq!(runner.worker().prompts.len(), 2);
        assert_eq!(runner.worker().prompts[0], "Original GH-601 task prompt");
        assert!(runner.worker().prompts[1].contains("Continue"));
        assert!(!runner.worker().prompts[1].contains("Original GH-601 task prompt"));
        assert_eq!(runner.worker().workspaces[0], runner.worker().workspaces[1]);
        assert_eq!(runner.worker().cleanup_calls, 1);
        assert_eq!(
            report.events,
            [
                AgentRunnerEvent::WorkspacePrepared {
                    path: runner.worker().workspaces[0].clone(),
                },
                AgentRunnerEvent::TurnStarted {
                    turn: 1,
                    prompt_kind: PromptKind::Initial,
                },
                AgentRunnerEvent::ContinuationRequested { turn: 1 },
                AgentRunnerEvent::TurnStarted {
                    turn: 2,
                    prompt_kind: PromptKind::Continuation,
                },
                AgentRunnerEvent::TurnCompleted { turn: 2 },
                AgentRunnerEvent::WorkerCleanedUp,
            ]
        );
    }

    #[test]
    fn worker_terminal_failures_forward_distinct_events_and_cleanup() {
        let cases = [
            (
                Ok(AppServerOutcome::Failed {
                    message: "worker failed".into(),
                }),
                AgentTerminalReason::WorkerFailed,
            ),
            (
                Ok(AppServerOutcome::TimedOut),
                AgentTerminalReason::WorkerTimedOut,
            ),
            (
                Err("launch failed".into()),
                AgentTerminalReason::WorkerLaunchFailed,
            ),
        ];

        for (outcome, expected_reason) in cases {
            let temp = TempDir::new().unwrap();
            let mut runner = AgentRunner::new(
                Workflow::parse("Prompt").expect("workflow"),
                WorkspaceManager::new(WorkspaceConfig {
                    root: temp.path().join("workspaces"),
                    preserve_success: true,
                }),
                AgentConfig {
                    max_attempts: 3,
                    max_turns: 1,
                    stall_timeout_secs: Some(900),
                },
                FakeWorker::new(vec![outcome]),
            );

            let report = runner.run_attempt(AgentRunRequest {
                task: task("601", "GH-601"),
                attempt: 1,
            });

            assert_eq!(report.result, RunResult::Failed);
            assert_eq!(report.terminal_reason, expected_reason);
            assert!(report.events.contains(&AgentRunnerEvent::TurnTerminal {
                turn: 1,
                reason: expected_reason,
            }));
            assert_eq!(runner.worker().cleanup_calls, 1);
        }
    }

    #[test]
    fn max_turns_returns_continuation_handoff_and_cleans_up_worker() {
        let temp = TempDir::new().unwrap();
        let mut runner = AgentRunner::new(
            Workflow::parse("Prompt").expect("workflow"),
            WorkspaceManager::new(WorkspaceConfig {
                root: temp.path().join("workspaces"),
                preserve_success: true,
            }),
            AgentConfig {
                max_attempts: 3,
                max_turns: 1,
                stall_timeout_secs: Some(900),
            },
            FakeWorker::new(vec![Ok(AppServerOutcome::InputRequired {
                prompt: Some("Need another turn".into()),
            })]),
        );

        let report = runner.run_attempt(AgentRunRequest {
            task: task("601", "GH-601"),
            attempt: 1,
        });

        assert_eq!(report.result, RunResult::NeedsContinuation);
        assert_eq!(report.terminal_reason, AgentTerminalReason::MaxTurns);
        assert!(report.events.contains(&AgentRunnerEvent::TurnTerminal {
            turn: 1,
            reason: AgentTerminalReason::MaxTurns,
        }));
        assert_eq!(runner.worker().prompts.len(), 1);
        assert_eq!(runner.worker().cleanup_calls, 1);
    }

    #[test]
    fn workspace_validation_failure_aborts_before_worker_launch() {
        let temp = TempDir::new().unwrap();
        let root_file = temp.path().join("not-a-directory");
        std::fs::write(&root_file, "not a workspace root").unwrap();
        let mut runner = AgentRunner::new(
            Workflow::parse("Prompt").expect("workflow"),
            WorkspaceManager::new(WorkspaceConfig {
                root: root_file,
                preserve_success: true,
            }),
            AgentConfig {
                max_attempts: 3,
                max_turns: 1,
                stall_timeout_secs: Some(900),
            },
            FakeWorker::new(vec![Ok(AppServerOutcome::TimedOut)]),
        );

        let report = runner.run_attempt(AgentRunRequest {
            task: task("601", "GH-601"),
            attempt: 1,
        });

        assert_eq!(report.result, RunResult::Failed);
        assert_eq!(
            report.terminal_reason,
            AgentTerminalReason::WorkspaceInvalid
        );
        assert!(report.events.is_empty());
        assert!(runner.worker().prompts.is_empty());
        assert_eq!(runner.worker().cleanup_calls, 0);
    }

    impl AgentWorker for FakeWorker {
        type Error = String;

        fn run_turn(
            &mut self,
            request: crate::symphony::app_server::AppServerRequest,
        ) -> Result<AppServerOutcome, Self::Error> {
            self.prompts.push(request.prompt);
            self.workspaces
                .push(request.workspace.display().to_string());
            self.outcomes.remove(0)
        }

        fn cleanup(&mut self) {
            self.cleanup_calls += 1;
        }
    }

    #[derive(Debug)]
    struct FakeWorker {
        outcomes: Vec<Result<AppServerOutcome, String>>,
        prompts: Vec<String>,
        workspaces: Vec<String>,
        cleanup_calls: usize,
    }

    impl FakeWorker {
        fn new(outcomes: Vec<Result<AppServerOutcome, String>>) -> Self {
            Self {
                outcomes,
                prompts: Vec::new(),
                workspaces: Vec::new(),
                cleanup_calls: 0,
            }
        }
    }

    fn task(id: &str, identifier: &str) -> NormalizedTask {
        NormalizedTask {
            id: id.into(),
            identifier: identifier.into(),
            title: "Agent runner".into(),
            body: "Body".into(),
            priority: None,
            state: "ready-for-agent".into(),
            branch: None,
            labels: Vec::new(),
            blockers: Vec::new(),
            url: None,
            path: None,
            created_at: None,
            updated_at: None,
            source: Default::default(),
        }
    }
}
