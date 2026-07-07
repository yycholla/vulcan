//! Session map and per-session state.
//!
//! Phase 3: each `SessionState` owns an optional warm Agent. The "main"
//! session is pre-created on daemon boot (without an Agent), then the
//! daemon startup path builds the Agent and installs it via
//! [`SessionState::set_agent`]. Additional sessions can be created
//! on-demand via `session.create`; their Agents are built lazily.
//!
//! The `"main"` session cannot be destroyed via
//! [`SessionMap::destroy_checked`] — it's the implicit default when a
//! request envelope omits or sends `"main"` for the `session` field.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::{Mutex, RwLock};
use tokio::sync::OwnedMutexGuard;
use tokio_util::sync::CancellationToken;

use crate::daemon::session_agent::{SessionAgentAssembler, SessionAgentOptions, SessionAgentSeed};

/// Shared, async-locked handle to a per-session Agent. `Arc` so a
/// single session can be locked from concurrent tasks (e.g. a
/// streaming `prompt.stream` background task and an inbound
/// `prompt.cancel` on the same session).
pub type AgentHandle = Arc<tokio::sync::Mutex<crate::agent::Agent>>;

/// Per-session state.
pub struct SessionState {
    pub id: String,
    pub parent_session_id: Option<String>,
    pub lineage_label: Option<String>,
    pub created_at: Instant,
    pub last_activity: Mutex<Instant>,
    pub in_flight: Mutex<bool>,
    turn_lock: Arc<tokio::sync::Mutex<()>>,
    active_turn_id: Mutex<Option<String>>,
    queued_turns: Mutex<usize>,
    pub cancel: CancellationToken,
    /// Phase 3: each session optionally owns its own warm Agent.
    /// `None` until the Agent is built (main at boot, others on
    /// create). Stored as `Arc<AsyncMutex<Agent>>` so callers can
    /// cheaply clone a handle and lock the Agent across `await`
    /// points without holding the outer `parking_lot::Mutex`.
    pub agent: Mutex<Option<AgentHandle>>,
    /// Per-turn cancel — clone of the installed agent's turn-cancel
    /// token. Cheap to fire; doesn't require locking the AsyncMutex.
    /// `None` until [`SessionState::set_agent`] installs an agent.
    /// `prompt.cancel` fires this directly so it doesn't deadlock
    /// against an in-flight `prompt.stream` that holds the AsyncMutex.
    pub agent_cancel: Mutex<Option<CancellationToken>>,
    /// Serializes lazy-build first-touches. Inner type is `()` — this
    /// Mutex exists purely to dedupe concurrent
    /// [`SessionState::ensure_agent`] callers down to one Agent build.
    /// The actual data swap goes through the parking_lot `agent`
    /// Mutex; the tokio mutex here is required so we can hold the
    /// lock across the `await` for `Agent::builder.build()`.
    build_lock: tokio::sync::Mutex<()>,
    /// Persistent build seed for this session's warm agent. Frontend
    /// event sinks stay per-request, but the max-iterations / tool profile /
    /// allowlist subset must survive config-driven rebuilds.
    agent_seed: Mutex<SessionAgentSeed>,
}

impl SessionState {
    pub fn new(id: String) -> Self {
        Self::new_with_lineage(id, None, None)
    }

    pub fn new_with_lineage(
        id: String,
        parent_session_id: Option<String>,
        lineage_label: Option<String>,
    ) -> Self {
        let now = Instant::now();
        Self {
            id,
            parent_session_id,
            lineage_label,
            created_at: now,
            last_activity: Mutex::new(now),
            in_flight: Mutex::new(false),
            turn_lock: Arc::new(tokio::sync::Mutex::new(())),
            active_turn_id: Mutex::new(None),
            queued_turns: Mutex::new(0),
            cancel: CancellationToken::new(),
            agent: Mutex::new(None),
            agent_cancel: Mutex::new(None),
            build_lock: tokio::sync::Mutex::new(()),
            agent_seed: Mutex::new(SessionAgentSeed::default()),
        }
    }

    /// Install a warm Agent into this session. Called by daemon startup
    /// for the "main" session, and by `session.create` for new sessions.
    /// Also captures a clone of the agent's per-turn cancel token so
    /// `prompt.cancel` can fire it without locking the AsyncMutex.
    pub fn set_agent(&self, agent: crate::agent::Agent) {
        *self.agent_cancel.lock() = Some(agent.cancel_handle());
        *self.agent.lock() = Some(Arc::new(tokio::sync::Mutex::new(agent)));
    }

    /// Replace the cancellation token for the currently active turn.
    /// `Agent::prepare_turn` swaps in a fresh token per turn, so the
    /// daemon must update this handle when it starts `prompt.run` or
    /// `prompt.stream`; otherwise `prompt.cancel` would keep firing the
    /// stale token captured when the Agent was installed.
    pub fn set_agent_cancel(&self, token: CancellationToken) {
        *self.agent_cancel.lock() = Some(token);
    }

    fn clear_agent_cancel(&self) {
        *self.agent_cancel.lock() = None;
    }

    pub async fn begin_turn(self: &Arc<Self>, turn_id: String) -> ActiveTurn {
        let lock = Arc::clone(&self.turn_lock);
        let has_waiters = *self.queued_turns.lock() > 0;
        let guard = if has_waiters {
            self.wait_for_turn(lock).await
        } else {
            match lock.clone().try_lock_owned() {
                Ok(guard) => guard,
                Err(_) => self.wait_for_turn(lock).await,
            }
        };

        *self.in_flight.lock() = true;
        *self.active_turn_id.lock() = Some(turn_id);
        self.touch();

        ActiveTurn {
            session: Arc::clone(self),
            _guard: guard,
        }
    }

    async fn wait_for_turn(&self, lock: Arc<tokio::sync::Mutex<()>>) -> OwnedMutexGuard<()> {
        *self.queued_turns.lock() += 1;
        let guard = lock.lock_owned().await;
        *self.queued_turns.lock() -= 1;
        guard
    }

    pub fn active_turn_id(&self) -> Option<String> {
        self.active_turn_id.lock().clone()
    }

    pub fn queued_turns(&self) -> usize {
        *self.queued_turns.lock()
    }

    /// Update `last_activity` to `Instant::now()`. Call when this
    /// session services any RPC; idle eviction reads this.
    pub fn touch(&self) {
        *self.last_activity.lock() = Instant::now();
    }

    /// True if this session has a warm Agent installed.
    pub fn has_agent(&self) -> bool {
        self.agent.lock().is_some()
    }

    /// Cloneable handle to the per-session Agent, if installed.
    /// Returns `None` if the session has no Agent yet (e.g. created
    /// via `session.create` for non-main; lazy-build is deferred).
    pub fn agent_arc(&self) -> Option<AgentHandle> {
        self.agent.lock().clone()
    }

    /// Cloneable handle to the agent's per-turn cancellation token,
    /// captured at `set_agent` time. Returns `None` if no agent is
    /// installed. Firing this token cancels the in-flight turn without
    /// locking the AsyncMutex.
    pub fn agent_cancel(&self) -> Option<CancellationToken> {
        self.agent_cancel.lock().clone()
    }

    /// Get this session's `AgentHandle`, building one inline if absent.
    ///
    /// Concurrent first-touches racing on the same session are
    /// serialized through `build_lock`: only one task performs the
    /// build, others wait on the lock and observe the just-installed
    /// Agent on the double-check. Build errors propagate to the
    /// caller; the next `ensure_agent` call will retry.
    ///
    /// This is the lazy-build path that makes non-`"main"` sessions
    /// usable without changing the wire protocol — `prompt.run`,
    /// `prompt.stream`, and `agent.*` handlers funnel through here.
    pub async fn ensure_agent(
        self: &Arc<Self>,
        assembler: &SessionAgentAssembler,
    ) -> anyhow::Result<AgentHandle> {
        self.ensure_agent_with_options(assembler, SessionAgentOptions::default())
            .await
    }

    /// Daemon child sessions can carry spawn-time tool profile,
    /// allowlist, and iteration cap before their Agent is installed.
    /// Existing sessions ignore these options once warm.
    pub async fn ensure_agent_with_options(
        self: &Arc<Self>,
        assembler: &SessionAgentAssembler,
        options: SessionAgentOptions,
    ) -> anyhow::Result<AgentHandle> {
        // Fast path: already installed.
        if let Some(handle) = self.agent_arc() {
            return Ok(handle);
        }

        // Slow path: serialize concurrent first-touches.
        let _build_guard = self.build_lock.lock().await;

        // Double-check: a racing task may have installed it while we
        // were waiting for the build lock.
        if let Some(handle) = self.agent_arc() {
            return Ok(handle);
        }

        let agent = assembler.assemble(options.clone()).await?;
        *self.agent_seed.lock() = options.seed();
        self.set_agent(agent);
        Ok(self.agent_arc().expect("just installed"))
    }

    /// Rebuild an already-warm agent using the session's stored build seed.
    /// Preserves the prior durable session history when it exists.
    pub async fn rebuild_agent(
        self: &Arc<Self>,
        assembler: &SessionAgentAssembler,
    ) -> anyhow::Result<()> {
        if self.agent_arc().is_none() {
            return Ok(());
        }

        let _build_guard = self.build_lock.lock().await;
        let Some(existing) = self.agent_arc() else {
            return Ok(());
        };

        let (prior_session_id, seed) = {
            let agent = existing.lock().await;
            (
                agent.session_id().to_string(),
                self.agent_seed.lock().clone(),
            )
        };

        let mut rebuilt = assembler.assemble(seed.into_options()).await?;
        if rebuilt
            .memory()
            .load_history(&prior_session_id)
            .await?
            .is_some()
        {
            rebuilt.resume_session(&prior_session_id).await?;
        }
        self.set_agent(rebuilt);
        Ok(())
    }
}

pub struct ActiveTurn {
    session: Arc<SessionState>,
    _guard: OwnedMutexGuard<()>,
}

impl Drop for ActiveTurn {
    fn drop(&mut self) {
        *self.session.in_flight.lock() = false;
        *self.session.active_turn_id.lock() = None;
        self.session.clear_agent_cancel();
        self.session.touch();
    }
}

/// Concurrent map of session id → state. Cheap reads under
/// `parking_lot::RwLock`; writes are infrequent (create/destroy).
pub struct SessionMap {
    inner: RwLock<HashMap<String, Arc<SessionState>>>,
}

impl SessionMap {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// Build a map with the default `"main"` session pre-created.
    pub fn with_main() -> Self {
        let map = Self::new();
        map.inner
            .write()
            .insert("main".into(), Arc::new(SessionState::new("main".into())));
        map
    }

    pub fn get(&self, id: &str) -> Option<Arc<SessionState>> {
        self.inner.read().get(id).cloned()
    }

    /// Insert a new session with the given id. Errors if the id is
    /// already present. Returns the id on success (convenience for
    /// chaining).
    pub fn create_named(&self, id: &str) -> anyhow::Result<String> {
        self.create_named_with_lineage(id, None, None)
    }

    pub fn create_named_with_lineage(
        &self,
        id: &str,
        parent_session_id: Option<String>,
        lineage_label: Option<String>,
    ) -> anyhow::Result<String> {
        let mut g = self.inner.write();
        if g.contains_key(id) {
            anyhow::bail!("session already exists: {id}");
        }
        g.insert(
            id.into(),
            Arc::new(SessionState::new_with_lineage(
                id.into(),
                parent_session_id,
                lineage_label,
            )),
        );
        Ok(id.into())
    }

    /// Remove a session unconditionally. Use [`Self::destroy_checked`] in
    /// production to guard `"main"`.
    pub fn destroy(&self, id: &str) {
        self.inner.write().remove(id);
    }

    /// Remove a session, refusing to destroy `"main"`.
    pub fn destroy_checked(&self, id: &str) -> anyhow::Result<()> {
        if id == "main" {
            anyhow::bail!("cannot destroy 'main' session");
        }
        self.destroy(id);
        Ok(())
    }

    /// Status snapshot for `daemon.status`. Each entry is a JSON
    /// object with id / in_flight / last_activity_secs_ago.
    pub fn descriptors(&self) -> Vec<serde_json::Value> {
        let g = self.inner.read();
        g.values()
            .map(|s| {
                let last = s.last_activity.lock();
                serde_json::json!({
                    "id": s.id,
                    "parent_session_id": s.parent_session_id,
                    "lineage_label": s.lineage_label,
                    "in_flight": *s.in_flight.lock(),
                    "active_turn_id": s.active_turn_id(),
                    "queued_turns": s.queued_turns(),
                    "last_activity_secs_ago": last.elapsed().as_secs(),
                    "has_agent": s.has_agent(),
                })
            })
            .collect()
    }

    /// True if any session has `in_flight == true`. Used by Task 0.10's
    /// config-watch loop to defer reload until idle.
    pub fn any_in_flight(&self) -> bool {
        let g = self.inner.read();
        g.values().any(|s| *s.in_flight.lock())
    }

    /// Snapshot all live sessions.
    pub fn all(&self) -> Vec<Arc<SessionState>> {
        self.inner.read().values().cloned().collect()
    }

    /// Count of sessions currently alive.
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// True if the map is empty. (Practically always false in
    /// production: `with_main` seeds the `"main"` session.)
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    /// List all session ids.
    pub fn ids(&self) -> Vec<String> {
        self.inner.read().keys().cloned().collect()
    }
}

impl Default for SessionMap {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal MockProvider-backed Agent for tests that need a
    /// stand-in without going through `Agent::builder` (which requires a
    /// real provider config / API key).
    async fn test_agent() -> crate::agent::Agent {
        use crate::hooks::HookRegistry;
        use crate::provider::{LLMProvider, Message, mock::MockProvider};
        use crate::skills::SkillRegistry;
        use crate::tools::ToolRegistry;
        use anyhow::Result;
        use async_trait::async_trait;
        use tokio_util::sync::CancellationToken;

        struct ProviderHandle(Arc<MockProvider>);
        #[async_trait]
        impl LLMProvider for ProviderHandle {
            async fn chat(
                &self,
                m: &[Message],
                t: &[crate::provider::ToolDefinition],
                c: CancellationToken,
            ) -> Result<crate::provider::ChatResponse> {
                self.0.chat(m, t, c).await
            }
            async fn chat_stream(
                &self,
                m: &[Message],
                t: &[crate::provider::ToolDefinition],
                tx: tokio::sync::mpsc::Sender<crate::provider::StreamEvent>,
                c: CancellationToken,
            ) -> Result<()> {
                self.0.chat_stream(m, t, tx, c).await
            }
            fn max_context(&self) -> usize {
                self.0.max_context()
            }
        }

        let mock = Arc::new(MockProvider::new(128_000));
        crate::agent::Agent::for_test(
            Box::new(ProviderHandle(mock)),
            ToolRegistry::new(),
            HookRegistry::new(),
            Arc::new(SkillRegistry::empty()),
        )
        .await
    }

    #[tokio::test]
    async fn ensure_agent_returns_existing_when_set() {
        let sess = Arc::new(SessionState::new("foo".into()));
        sess.set_agent(test_agent().await);
        let assembler =
            SessionAgentAssembler::new(Arc::new(crate::config::Config::default()), None);
        let h = sess.ensure_agent(&assembler).await.unwrap();
        let h2 = sess.agent_arc().unwrap();
        assert!(
            Arc::ptr_eq(&h, &h2),
            "ensure_agent returns the existing handle"
        );
    }

    #[test]
    fn active_agent_cancel_token_can_be_replaced_per_turn() {
        let sess = SessionState::new("foo".into());
        let stale = CancellationToken::new();
        let active = CancellationToken::new();

        *sess.agent_cancel.lock() = Some(stale.clone());
        sess.set_agent_cancel(active.clone());

        let token = sess.agent_cancel().expect("cancel token installed");
        token.cancel();

        assert!(
            active.is_cancelled(),
            "prompt.cancel must fire the active turn token"
        );
        assert!(
            !stale.is_cancelled(),
            "replacing the active token must not keep cancelling stale turns"
        );
    }

    #[tokio::test]
    async fn turn_guard_tracks_active_and_queued_status() {
        let sess = Arc::new(SessionState::new("foo".into()));
        let first = sess.begin_turn("turn-1".into()).await;

        assert_eq!(sess.active_turn_id(), Some("turn-1".to_string()));
        assert_eq!(sess.queued_turns(), 0);
        assert!(*sess.in_flight.lock());

        let waiting = {
            let sess = Arc::clone(&sess);
            tokio::spawn(async move {
                let _second = sess.begin_turn("turn-2".into()).await;
                assert_eq!(sess.active_turn_id(), Some("turn-2".to_string()));
            })
        };
        tokio::task::yield_now().await;
        assert_eq!(sess.queued_turns(), 1);

        drop(first);
        waiting.await.expect("queued turn finishes");
        assert_eq!(sess.active_turn_id(), None);
        assert_eq!(sess.queued_turns(), 0);
        assert!(!*sess.in_flight.lock());
    }

    #[test]
    fn map_with_main_has_main_session() {
        let map = SessionMap::with_main();
        assert!(map.get("main").is_some());
        assert!(map.get("nonexistent").is_none());
    }

    #[test]
    fn create_named_inserts_session() {
        let map = SessionMap::with_main();
        let id = map.create_named("foo").unwrap();
        assert_eq!(id, "foo");
        assert!(map.get("foo").is_some());
    }

    #[test]
    fn create_named_rejects_duplicate() {
        let map = SessionMap::with_main();
        map.create_named("foo").unwrap();
        let err = map.create_named("foo").expect_err("must reject duplicate");
        assert!(err.to_string().contains("foo"), "error mentions session id");
    }

    #[test]
    fn destroy_removes_session() {
        let map = SessionMap::with_main();
        map.create_named("foo").unwrap();
        map.destroy("foo");
        assert!(map.get("foo").is_none());
    }

    #[test]
    fn destroy_checked_rejects_main() {
        let map = SessionMap::with_main();
        let err = map
            .destroy_checked("main")
            .expect_err("main is undeletable");
        assert!(err.to_string().contains("main"));
        assert!(map.get("main").is_some(), "main still present");
    }

    #[test]
    fn destroy_checked_allows_others() {
        let map = SessionMap::with_main();
        map.create_named("foo").unwrap();
        map.destroy_checked("foo").unwrap();
        assert!(map.get("foo").is_none());
    }

    #[test]
    fn descriptors_includes_main() {
        let map = SessionMap::with_main();
        let d = map.descriptors();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0]["id"], "main");
        assert_eq!(d[0]["active_turn_id"], serde_json::Value::Null);
        assert_eq!(d[0]["queued_turns"], 0);
    }

    #[test]
    fn any_in_flight_false_when_idle() {
        let map = SessionMap::with_main();
        assert!(!map.any_in_flight());
    }

    #[test]
    fn any_in_flight_true_when_session_busy() {
        let map = SessionMap::with_main();
        *map.get("main").unwrap().in_flight.lock() = true;
        assert!(map.any_in_flight());
    }

    #[test]
    fn len_counts_correctly() {
        let map = SessionMap::with_main();
        assert_eq!(map.len(), 1);
        map.create_named("foo").unwrap();
        assert_eq!(map.len(), 2);
    }
}
