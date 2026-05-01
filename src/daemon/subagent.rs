//! Daemon-backed implementation of `spawn_subagent`.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::daemon::state::DaemonState;
use crate::tools::spawn::{SubagentRunOutput, SubagentRunRequest, SubagentRunner};

pub struct DaemonSubagentRunner {
    state: Arc<DaemonState>,
}

impl DaemonSubagentRunner {
    pub fn new(state: Arc<DaemonState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl SubagentRunner for DaemonSubagentRunner {
    async fn run_subagent(
        &self,
        request: SubagentRunRequest,
        cancel: CancellationToken,
    ) -> Result<SubagentRunOutput> {
        let child_session_id = request.child_id.to_string();
        let lineage_label = Some(format!(
            "spawn_subagent: {}",
            request.task.chars().take(80).collect::<String>()
        ));
        self.state.sessions().create_named_with_lineage(
            &child_session_id,
            request.parent_session_id.clone(),
            lineage_label,
        )?;
        if let (Some(pool), Some(parent_session_id)) =
            (self.state.pool(), request.parent_session_id.as_deref())
        {
            let active_extension_ids = pool
                .extension_registry()
                .list()
                .into_iter()
                .filter(|m| m.status == crate::extensions::ExtensionStatus::Active)
                .map(|m| m.id)
                .collect::<Vec<_>>();
            pool.extension_state_store().branch_session(
                parent_session_id,
                &child_session_id,
                &active_extension_ids,
            )?;
        }

        let sess = self
            .state
            .sessions()
            .get(&child_session_id)
            .ok_or_else(|| anyhow::anyhow!("child session was not created: {child_session_id}"))?;
        let agent_arc = sess
            .ensure_agent_with_options(
                self.state.config(),
                self.state.pool().cloned(),
                Some(request.max_iterations),
                request.profile_name.clone(),
                Some(&request.allowed_tools),
                crate::extensions::FrontendCapability::full_set(),
                Vec::new(),
            )
            .await?;

        sess.touch();
        *sess.in_flight.lock() = true;
        sess.set_agent_cancel(cancel.clone());
        let mut child = agent_arc.lock().await;
        let nested_runner = Arc::new(DaemonSubagentRunner::new(Arc::clone(&self.state)));
        child.install_subagent_runner(
            Arc::new(self.state.config().clone()),
            child_session_id.clone(),
            nested_runner,
        );
        let run_result = match request.parent_run_id {
            Some(parent_run_id) => {
                child
                    .run_prompt_with_cancel_origin(
                        &request.task,
                        cancel.clone(),
                        crate::run_record::RunOrigin::Subagent { parent_run_id },
                    )
                    .await
            }
            None => {
                child
                    .run_prompt_with_cancel(&request.task, cancel.clone())
                    .await
            }
        };
        let iterations = child.iterations();
        let tokens_consumed = child.tokens_consumed();
        drop(child);
        *sess.in_flight.lock() = false;
        sess.touch();

        if let Some(pool) = self.state.pool() {
            pool.extension_state_store()
                .reap_session(&child_session_id)?;
        }
        let final_text = run_result?;
        Ok(SubagentRunOutput {
            final_text,
            iterations,
            tokens_consumed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::ChildAgentId;
    use uuid::Uuid;

    #[tokio::test]
    async fn daemon_runner_creates_child_session_with_lineage_before_running() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        let runner = DaemonSubagentRunner::new(Arc::clone(&state));
        let child_id = ChildAgentId(Uuid::new_v4());
        let result = runner
            .run_subagent(
                SubagentRunRequest {
                    child_id,
                    parent_session_id: Some("main".into()),
                    parent_run_id: None,
                    task: "inspect session wiring".into(),
                    allowed_tools: vec!["read_file".into()],
                    profile_name: None,
                    max_iterations: 2,
                },
                CancellationToken::new(),
            )
            .await;

        assert!(
            result.is_err(),
            "minimal test config should fail child agent build"
        );
        let child = state
            .sessions()
            .get(&child_id.to_string())
            .expect("child session created before agent run");
        assert_eq!(child.parent_session_id.as_deref(), Some("main"));
        assert_eq!(
            child.lineage_label.as_deref(),
            Some("spawn_subagent: inspect session wiring")
        );
    }
}
