use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpServerHealth {
    #[default]
    Stopped,
    Starting,
    Healthy,
    Unhealthy,
    Restarting,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct McpRestartPolicy {
    pub max_restarts: u32,
    pub backoff_secs: u64,
}

impl Default for McpRestartPolicy {
    fn default() -> Self {
        Self {
            max_restarts: 3,
            backoff_secs: 2,
        }
    }
}

impl McpRestartPolicy {
    pub fn backoff(&self) -> Duration {
        Duration::from_secs(self.backoff_secs.max(1))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSupervisorSnapshot {
    pub server_id: String,
    pub health: McpServerHealth,
    pub restart_attempts: u32,
    pub last_error: Option<String>,
    pub next_restart_after: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct McpSupervisorState {
    server_id: String,
    policy: McpRestartPolicy,
    health: McpServerHealth,
    restart_attempts: u32,
    last_error: Option<String>,
    next_restart_after: Option<Duration>,
}

impl McpSupervisorState {
    pub fn new(server_id: impl Into<String>, policy: McpRestartPolicy) -> Self {
        Self {
            server_id: server_id.into(),
            policy,
            health: McpServerHealth::Stopped,
            restart_attempts: 0,
            last_error: None,
            next_restart_after: None,
        }
    }

    pub fn disabled(server_id: impl Into<String>, policy: McpRestartPolicy) -> Self {
        let mut state = Self::new(server_id, policy);
        state.health = McpServerHealth::Disabled;
        state
    }

    pub fn mark_starting(&mut self) {
        self.health = McpServerHealth::Starting;
        self.next_restart_after = None;
    }

    pub fn mark_healthy(&mut self) {
        self.health = McpServerHealth::Healthy;
        self.restart_attempts = 0;
        self.last_error = None;
        self.next_restart_after = None;
    }

    pub fn mark_stopped(&mut self) {
        self.health = McpServerHealth::Stopped;
        self.next_restart_after = None;
    }

    pub fn record_crash(&mut self, error: impl Into<String>) -> bool {
        self.last_error = Some(error.into());
        if self.restart_attempts >= self.policy.max_restarts {
            self.health = McpServerHealth::Unhealthy;
            self.next_restart_after = None;
            return false;
        }
        self.restart_attempts += 1;
        self.health = McpServerHealth::Restarting;
        self.next_restart_after = Some(self.policy.backoff() * self.restart_attempts);
        true
    }

    pub fn snapshot(&self) -> McpSupervisorSnapshot {
        McpSupervisorSnapshot {
            server_id: self.server_id.clone(),
            health: self.health,
            restart_attempts: self.restart_attempts,
            last_error: self.last_error.clone(),
            next_restart_after: self.next_restart_after,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_restart_is_bounded_with_backoff() {
        let mut state = McpSupervisorState::new(
            "fake",
            McpRestartPolicy {
                max_restarts: 2,
                backoff_secs: 3,
            },
        );
        assert!(state.record_crash("boom 1"));
        assert_eq!(state.snapshot().health, McpServerHealth::Restarting);
        assert_eq!(
            state.snapshot().next_restart_after,
            Some(Duration::from_secs(3))
        );

        assert!(state.record_crash("boom 2"));
        assert_eq!(
            state.snapshot().next_restart_after,
            Some(Duration::from_secs(6))
        );

        assert!(!state.record_crash("boom 3"));
        assert_eq!(state.snapshot().health, McpServerHealth::Unhealthy);
        assert_eq!(state.snapshot().restart_attempts, 2);
    }

    #[test]
    fn healthy_reset_clears_restart_budget() {
        let mut state = McpSupervisorState::new("fake", McpRestartPolicy::default());
        assert!(state.record_crash("boom"));
        state.mark_healthy();
        let snapshot = state.snapshot();
        assert_eq!(snapshot.health, McpServerHealth::Healthy);
        assert_eq!(snapshot.restart_attempts, 0);
        assert!(snapshot.last_error.is_none());
    }
}
