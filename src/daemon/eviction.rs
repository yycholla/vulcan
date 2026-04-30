//! Background sweeper that evicts idle non-`"main"` sessions
//! (YYC-266 Slice 3, Task 3.2).
//!
//! Each tick: snapshot session ids, compute idle duration per session,
//! evict any non-`"main"` session that is `!in_flight && last_activity
//! elapsed > ttl`. Eviction also fires the session's `agent_cancel`
//! token (defensive — at this point `in_flight` should be false anyway).
//!
//! The sweeper observes [`DaemonState::shutdown_signal`] and exits when
//! shutdown latches.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::interval;

use crate::daemon::state::DaemonState;

/// Spawn the eviction loop. Returns a `JoinHandle` so callers can
/// await shutdown if they want to. The loop exits on the watch-based
/// shutdown signal latched by [`DaemonState::signal_shutdown`].
pub fn spawn(
    state: Arc<DaemonState>,
    idle_ttl: Duration,
    sweep_interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(eviction_loop(state, idle_ttl, sweep_interval))
}

async fn eviction_loop(state: Arc<DaemonState>, idle_ttl: Duration, sweep_interval: Duration) {
    let mut shutdown = state.shutdown_signal();
    let mut ticker = interval(sweep_interval);
    // Skip the immediate first tick — we want to wait one interval
    // before sweeping so a freshly-started daemon isn't immediately
    // evicting newly-created sessions.
    ticker.tick().await;

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let evicted = evict_idle(&state, idle_ttl);
                if !evicted.is_empty() {
                    tracing::info!(
                        count = evicted.len(),
                        sessions = ?evicted,
                        "daemon: evicted idle sessions",
                    );
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    tracing::debug!("daemon: eviction loop observed shutdown");
                    return;
                }
            }
        }
    }
}

/// Sweep over the SessionMap once and evict any non-`"main"`
/// session whose `last_activity` is older than `idle_ttl` and
/// whose `in_flight` flag is false. Fires `agent_cancel`
/// defensively before destroy. Returns the ids evicted (helpful
/// for tests + logging).
pub(crate) fn evict_idle(state: &DaemonState, idle_ttl: Duration) -> Vec<String> {
    let mut evicted = Vec::new();
    for id in state.sessions().ids() {
        if id == "main" {
            continue;
        }
        let Some(sess) = state.sessions().get(&id) else {
            continue;
        };
        if *sess.in_flight.lock() {
            continue;
        }
        let elapsed = sess.last_activity.lock().elapsed();
        if elapsed > idle_ttl {
            // Defensive: fire agent cancel before removing the
            // session. At this point `in_flight` is false so any
            // turn that was running has already finished, but
            // firing the token is cheap and means a racing
            // streamer that flips `in_flight` between our check
            // and `destroy` still observes cancellation.
            if let Some(token) = sess.agent_cancel() {
                token.cancel();
            }
            state.sessions().destroy(&id);
            evicted.push(id);
        }
    }
    evicted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::state::DaemonState;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    /// Helper: backdate `last_activity` so the session looks idle.
    /// `Instant::checked_sub` may return `None` on a freshly-started
    /// process; tests pick `idle_for` small enough that this doesn't
    /// trip on a green CI runner.
    fn make_idle(state: &DaemonState, id: &str, idle_for: Duration) {
        let sess = state.sessions().get(id).unwrap();
        let now = std::time::Instant::now();
        let backdated = now
            .checked_sub(idle_for)
            .expect("system clock advanced enough");
        *sess.last_activity.lock() = backdated;
    }

    #[tokio::test]
    async fn evicts_idle_non_main_session() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        state.sessions().create_named("foo").unwrap();
        make_idle(&state, "foo", Duration::from_secs(10));
        let evicted = evict_idle(&state, Duration::from_secs(5));
        assert_eq!(evicted, vec!["foo".to_string()]);
        assert!(state.sessions().get("foo").is_none(), "foo evicted");
    }

    #[tokio::test]
    async fn does_not_evict_main() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        // Use a small backdate so `Instant::checked_sub` is safe on a
        // freshly-started CI runner. The exemption logic doesn't
        // depend on the magnitude of idleness.
        make_idle(&state, "main", Duration::from_secs(10));
        let evicted = evict_idle(&state, Duration::from_secs(5));
        assert!(evicted.is_empty(), "main is exempt");
        assert!(state.sessions().get("main").is_some());
    }

    #[tokio::test]
    async fn does_not_evict_in_flight_session() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        state.sessions().create_named("busy").unwrap();
        make_idle(&state, "busy", Duration::from_secs(10));
        // Mark in flight despite being idle.
        *state.sessions().get("busy").unwrap().in_flight.lock() = true;
        let evicted = evict_idle(&state, Duration::from_secs(5));
        assert!(evicted.is_empty(), "in-flight session must not be evicted");
        assert!(state.sessions().get("busy").is_some());
    }

    #[tokio::test]
    async fn does_not_evict_active_session() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        state.sessions().create_named("fresh").unwrap();
        // last_activity is "now" by default.
        let evicted = evict_idle(&state, Duration::from_secs(5));
        assert!(evicted.is_empty(), "fresh session retained");
    }

    #[tokio::test]
    async fn fires_agent_cancel_on_eviction() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        state.sessions().create_named("doomed").unwrap();
        let sess = state.sessions().get("doomed").unwrap();
        let token = CancellationToken::new();
        *sess.agent_cancel.lock() = Some(token.clone());
        make_idle(&state, "doomed", Duration::from_secs(10));
        evict_idle(&state, Duration::from_secs(5));
        assert!(
            token.is_cancelled(),
            "evicted session's agent_cancel must fire",
        );
    }

    #[tokio::test]
    async fn loop_exits_on_shutdown() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        let handle = spawn(
            state.clone(),
            Duration::from_secs(60),
            Duration::from_millis(50),
        );
        // Briefly let the loop run.
        tokio::time::sleep(Duration::from_millis(100)).await;
        state.signal_shutdown();
        let exited = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(
            exited.is_ok(),
            "eviction loop must exit within 2s of shutdown",
        );
    }

    #[tokio::test]
    async fn loop_evicts_on_tick() {
        let state = Arc::new(DaemonState::for_tests_minimal());
        state.sessions().create_named("aged").unwrap();
        make_idle(&state, "aged", Duration::from_secs(10));
        let _handle = spawn(
            state.clone(),
            Duration::from_secs(5),    // ttl
            Duration::from_millis(50), // sweep interval
        );
        // Wait for at least 2 ticks (skip + first sweep).
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            state.sessions().get("aged").is_none(),
            "loop should have evicted within 150ms",
        );
        state.signal_shutdown();
    }
}
