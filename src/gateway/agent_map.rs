//! Per-lane Agent cache for the gateway daemon.
//!
//! `AgentMap` keys live `Agent` instances by `LaneKey` so each chat (Slack
//! thread, IRC channel, Matrix room, etc.) gets a long-lived agent with its
//! own hook state. First touch on a lane spawns the Agent; subsequent calls
//! reuse it. Eviction lands in Task 9 — for now, the map grows monotonically.
//!
//! The double-checked spawn pattern in `get_or_spawn` matches the lane router
//! in `lane.rs`: read-lock → write-lock → recheck → insert.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::{Mutex, RwLock};

use crate::agent::Agent;
use crate::config::Config;
use crate::gateway::lane::LaneKey;
use crate::hooks::HookRegistry;
use crate::hooks::audit::AuditHook;

/// Capacity of the per-lane audit ring. Matches the TUI's default in
/// `src/tui/mod.rs:384`.
const AUDIT_BUFFER_CAPACITY: usize = 200;

pub struct AgentMap {
    inner: Arc<RwLock<HashMap<LaneKey, LaneEntry>>>,
    config: Arc<Config>,
    #[allow(dead_code)] // Consumed by Task 9's evictor.
    idle_ttl: Duration,
}

pub(crate) struct LaneEntry {
    pub agent: Arc<Mutex<Agent>>,
    #[allow(dead_code)] // Stored for observability + Task 9 rehydration.
    pub session_id: String,
    pub last_activity: Instant,
}

impl AgentMap {
    pub fn new(config: Arc<Config>, idle_ttl: Duration) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            config,
            idle_ttl,
        }
    }

    /// Returns the Agent for `lane`, building one on first call. Bumps
    /// `last_activity` on every call so the Task 9 evictor sees liveness.
    pub async fn get_or_spawn(&self, lane: &LaneKey) -> Result<Arc<Mutex<Agent>>> {
        // Fast path: read-lock probe. Drop the read guard before re-acquiring
        // as a writer to bump `last_activity`.
        {
            let map = self.inner.read().await;
            if map.contains_key(lane) {
                drop(map);
                let mut map = self.inner.write().await;
                if let Some(entry) = map.get_mut(lane) {
                    entry.last_activity = Instant::now();
                    return Ok(Arc::clone(&entry.agent));
                }
                // Fell through: a concurrent evictor (future) removed the
                // entry between our drop and re-acquire. Treat as a miss.
            }
        }

        // Slow path: write-lock + double-check + spawn.
        let mut map = self.inner.write().await;
        if let Some(entry) = map.get_mut(lane) {
            entry.last_activity = Instant::now();
            return Ok(Arc::clone(&entry.agent));
        }

        // No pause channel — gateway has no interactive surface; Task 18
        // swaps approval for auto-deny before any real traffic flows.
        let mut hook_reg = HookRegistry::new();
        let (audit_hook, _audit_buf) = AuditHook::new(AUDIT_BUFFER_CAPACITY);
        hook_reg.register(audit_hook);

        let agent = Agent::with_hooks_and_pause(&self.config, hook_reg, None).await?;
        let agent = Arc::new(Mutex::new(agent));
        agent.lock().await.start_session().await;

        let session_id = derive_session_id(lane);
        map.insert(
            lane.clone(),
            LaneEntry {
                agent: Arc::clone(&agent),
                session_id,
                last_activity: Instant::now(),
            },
        );
        Ok(agent)
    }

    /// Count of currently-active lanes. Used by GET /v1/lanes (Task 15) and
    /// tests.
    pub async fn active_lanes(&self) -> usize {
        self.inner.read().await.len()
    }

    /// Internal accessor for tests + the future evictor (Task 9).
    #[allow(dead_code)]
    pub(crate) fn inner(&self) -> Arc<RwLock<HashMap<LaneKey, LaneEntry>>> {
        Arc::clone(&self.inner)
    }
}

/// Derive a stable session id for a lane. Used so future AgentMap eviction
/// + rehydration (Task 9) can find the right SessionStore row.
pub fn derive_session_id(lane: &LaneKey) -> String {
    format!("gateway:{}:{}", lane.platform, lane.chat_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn test_config() -> Arc<Config> {
        Arc::new(Config::default())
    }

    fn test_lane(chat: &str) -> LaneKey {
        LaneKey {
            platform: "loopback".into(),
            chat_id: chat.into(),
        }
    }

    #[test]
    fn session_id_is_deterministic_per_lane() {
        let a = derive_session_id(&test_lane("42"));
        let b = derive_session_id(&test_lane("42"));
        assert_eq!(a, b);
        assert_eq!(a, "gateway:loopback:42");
        let c = derive_session_id(&test_lane("99"));
        assert_ne!(a, c);
    }

    // Ignored: `Agent::with_hooks_and_pause` bails when no API key is set,
    // and `Config::default()` provides none. A future MockProvider /
    // builder-style test config would let these run unconditionally;
    // tracking this gap in the Task 8 report.
    #[tokio::test]
    #[ignore = "needs API key or MockProvider; Config::default() has no api_key"]
    async fn second_get_reuses_agent() {
        let map = AgentMap::new(test_config(), Duration::from_secs(60));
        let lane = test_lane("x");
        let a1 = map.get_or_spawn(&lane).await.expect("first spawn");
        let a2 = map.get_or_spawn(&lane).await.expect("second get");
        assert!(Arc::ptr_eq(&a1, &a2), "same lane must return the same Arc");
        assert_eq!(map.active_lanes().await, 1);
    }

    #[tokio::test]
    #[ignore = "needs API key or MockProvider; Config::default() has no api_key"]
    async fn distinct_lanes_get_distinct_agents() {
        let map = AgentMap::new(test_config(), Duration::from_secs(60));
        let a = map.get_or_spawn(&test_lane("a")).await.expect("a");
        let b = map.get_or_spawn(&test_lane("b")).await.expect("b");
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(map.active_lanes().await, 2);
    }
}
