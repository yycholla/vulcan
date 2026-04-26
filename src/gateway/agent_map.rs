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
use tokio::task::JoinHandle;

use crate::agent::Agent;
use crate::config::Config;
use crate::gateway::lane::LaneKey;
use crate::hooks::HookRegistry;
use crate::hooks::audit::{AuditBuffer, AuditHook};

/// Capacity of the per-lane audit ring. Matches the TUI's default in
/// `src/tui/mod.rs:384`.
const AUDIT_BUFFER_CAPACITY: usize = 200;

pub struct AgentMap {
    inner: Arc<RwLock<HashMap<LaneKey, LaneEntry>>>,
    config: Arc<Config>,
    idle_ttl: Duration,
}

pub(crate) struct LaneEntry {
    pub agent: Arc<Mutex<Agent>>,
    #[allow(dead_code)] // Stored for observability + Task 9 rehydration.
    pub session_id: String,
    #[allow(dead_code)] // Surfaced by GET /v1/lanes (Task 15).
    pub audit_buf: AuditBuffer,
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

        // Slow path: build the Agent OUTSIDE the map's write lock so a slow
        // cold spawn on one lane doesn't block first-touches on every other
        // lane. Acquire the write lock only briefly to insert.
        //
        // ApprovalHook is registered inside `with_hooks_and_pause` regardless
        // of `pause_tx`. With `None` here, any non-`Always` approval mode in
        // user config will block the lane on first prompt — Task 18 wires an
        // auto-deny variant that closes that gap.
        let mut hook_reg = HookRegistry::new();
        let (audit_hook, audit_buf) = AuditHook::new(AUDIT_BUFFER_CAPACITY);
        hook_reg.register(audit_hook);

        let agent = Agent::with_hooks_and_pause(&self.config, hook_reg, None).await?;
        let agent = Arc::new(Mutex::new(agent));
        agent.lock().await.start_session().await;

        // Triple-check: another task may have spawned the same lane while we
        // were building. If so, drop our agent and adopt theirs.
        let mut map = self.inner.write().await;
        if let Some(entry) = map.get_mut(lane) {
            entry.last_activity = Instant::now();
            return Ok(Arc::clone(&entry.agent));
        }

        let session_id = derive_session_id(lane);
        map.insert(
            lane.clone(),
            LaneEntry {
                agent: Arc::clone(&agent),
                session_id,
                audit_buf,
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

    /// Spawn a background task that periodically evicts lanes idle longer
    /// than `self.idle_ttl`. The returned handle aborts the loop on drop or
    /// on `.abort()`.
    pub fn spawn_evictor(&self) -> JoinHandle<()> {
        const SCAN_INTERVAL: Duration = Duration::from_secs(60);
        let inner = Arc::clone(&self.inner);
        let ttl = self.idle_ttl;
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(SCAN_INTERVAL);
            // Skip the immediate first tick; the first eviction happens after
            // one full interval.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                evict_idle(&inner, ttl).await;
            }
        })
    }

    #[cfg(test)]
    pub(crate) async fn insert_for_test(
        &self,
        lane: LaneKey,
        agent: Arc<Mutex<Agent>>,
        audit_buf: AuditBuffer,
        last_activity: Instant,
    ) {
        let session_id = derive_session_id(&lane);
        let mut map = self.inner.write().await;
        map.insert(
            lane,
            LaneEntry {
                agent,
                session_id,
                audit_buf,
                last_activity,
            },
        );
    }
}

pub(crate) async fn evict_idle(
    inner: &Arc<RwLock<HashMap<LaneKey, LaneEntry>>>,
    ttl: Duration,
) {
    // Build the to-evict list under the read lock; do the actual removal +
    // end_session under the write lock + outside the map. This keeps the
    // write lock window tight even if many lanes age out at once.
    let now = Instant::now();
    let candidates: Vec<LaneKey> = {
        let map = inner.read().await;
        map.iter()
            .filter(|(_, entry)| now.duration_since(entry.last_activity) > ttl)
            .map(|(k, _)| k.clone())
            .collect()
    };
    if candidates.is_empty() {
        return;
    }
    let evicted = {
        let mut map = inner.write().await;
        let mut taken = Vec::with_capacity(candidates.len());
        for lane in &candidates {
            if let Some(entry) = map.get(lane) {
                // Re-check liveness — a concurrent get_or_spawn may have
                // bumped last_activity between our snapshot and the write
                // lock.
                if now.duration_since(entry.last_activity) > ttl {
                    if let Some(entry) = map.remove(lane) {
                        taken.push((lane.clone(), entry));
                    }
                }
            }
        }
        taken
    };
    for (lane, entry) in evicted {
        let agent = entry.agent.lock().await;
        agent.end_session().await;
        tracing::info!(target: "gateway::agent_map",
            platform = lane.platform.as_str(),
            chat_id = lane.chat_id.as_str(),
            "evicted idle lane");
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
    use crate::provider::mock::MockProvider;
    use crate::provider::{ChatResponse, LLMProvider, Message, StreamEvent, ToolDefinition};
    use crate::skills::SkillRegistry;
    use crate::tools::ToolRegistry;
    use tokio_util::sync::CancellationToken;

    fn test_config() -> Arc<Config> {
        Arc::new(Config::default())
    }

    fn test_lane(chat: &str) -> LaneKey {
        LaneKey {
            platform: "loopback".into(),
            chat_id: chat.into(),
        }
    }

    /// Build an Agent backed by `MockProvider` so eviction tests don't need
    /// an API key. Mirrors `agent::tests::agent_with_mock`'s `ProviderHandle`
    /// shim.
    fn build_test_agent() -> Arc<Mutex<Agent>> {
        struct ProviderHandle(Arc<MockProvider>);
        #[async_trait::async_trait]
        impl LLMProvider for ProviderHandle {
            async fn chat(
                &self,
                m: &[Message],
                t: &[ToolDefinition],
                c: CancellationToken,
            ) -> Result<ChatResponse> {
                self.0.chat(m, t, c).await
            }
            async fn chat_stream(
                &self,
                m: &[Message],
                t: &[ToolDefinition],
                tx: tokio::sync::mpsc::UnboundedSender<StreamEvent>,
                c: CancellationToken,
            ) -> Result<()> {
                self.0.chat_stream(m, t, tx, c).await
            }
            fn max_context(&self) -> usize {
                self.0.max_context()
            }
        }
        let mock = Arc::new(MockProvider::new(128_000));
        let skills = Arc::new(SkillRegistry::new(&std::path::PathBuf::from(
            "/tmp/vulcan-test-skills-nonexistent",
        )));
        let agent = Agent::for_test(
            Box::new(ProviderHandle(mock)),
            ToolRegistry::new(),
            HookRegistry::new(),
            skills,
        );
        Arc::new(Mutex::new(agent))
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

    #[tokio::test(start_paused = true)]
    #[ignore = "needs API key or MockProvider; Agent::with_hooks_and_pause + Config::default() has no api_key"]
    async fn idle_lanes_evicted_after_ttl() {
        let map = AgentMap::new(test_config(), Duration::from_secs(60));
        let evictor = map.spawn_evictor();
        let lane = test_lane("x");
        map.get_or_spawn(&lane).await.expect("spawn");
        assert_eq!(map.active_lanes().await, 1);

        // Advance well past TTL + scan interval (60s + 60s = 120s minimum).
        tokio::time::advance(Duration::from_secs(180)).await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        assert_eq!(map.active_lanes().await, 0, "lane should be evicted");
        evictor.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn evict_idle_removes_entries_past_ttl() {
        // Advance the paused clock first so `Instant::now() - 120s` doesn't
        // underflow at runtime startup.
        tokio::time::advance(Duration::from_secs(200)).await;

        let map = AgentMap::new(test_config(), Duration::from_secs(60));
        let lane = test_lane("stale");
        let agent = build_test_agent();
        let (_h, audit_buf) = AuditHook::new(8);
        let stale_when = Instant::now() - Duration::from_secs(120);
        map.insert_for_test(lane.clone(), agent, audit_buf, stale_when)
            .await;
        assert_eq!(map.active_lanes().await, 1);

        super::evict_idle(&map.inner, Duration::from_secs(60)).await;
        assert_eq!(map.active_lanes().await, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn evict_idle_keeps_recent_entries() {
        let map = AgentMap::new(test_config(), Duration::from_secs(60));
        let lane = test_lane("fresh");
        let agent = build_test_agent();
        let (_h, audit_buf) = AuditHook::new(8);
        map.insert_for_test(lane.clone(), agent, audit_buf, Instant::now())
            .await;
        assert_eq!(map.active_lanes().await, 1);

        super::evict_idle(&map.inner, Duration::from_secs(60)).await;
        assert_eq!(map.active_lanes().await, 1);
    }
}
