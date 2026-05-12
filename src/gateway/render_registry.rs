//! In-memory map from (platform, chat_id, turn_id) → platform message id.
//!
//! StreamRenderer (PR-2a) writes the first chunk's anchor into this
//! registry once OutboundDispatcher captures it; subsequent chunks
//! read the anchor and emit OutboundMessages with `edit_target`
//! populated so the dispatcher routes to `Platform::edit`.
//!
//! Bound: the registry keeps at most [`DEFAULT_RENDER_REGISTRY_CAPACITY`]
//! anchors and evicts the least-recently-used entry on overflow. A turn
//! should still call [`RenderRegistry::forget`] when it ends; the LRU cap is
//! the daemon-mode safety net for crashed, interrupted, or otherwise missed
//! cleanup paths.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

/// Hard cap for in-memory render anchors. Anchors are tiny, but gateway mode
/// can see unbounded platform/chat/turn ids over time; 512 concurrent or
/// recently-interrupted turns is comfortably above normal operation while
/// keeping retention deterministic.
pub const DEFAULT_RENDER_REGISTRY_CAPACITY: usize = 512;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RenderKey {
    pub platform: String,
    pub chat_id: String,
    /// PR-2a stand-in: callers populate this with `chat_id` because the
    /// worker hasn't yet been switched to streaming and there's no real
    /// per-turn id surfaced to the dispatcher. PR-2b adds a real
    /// `turn_id` column on `outbound_queue` and threads it through.
    pub turn_id: String,
}

pub struct RenderRegistry {
    inner: Arc<RwLock<RenderRegistryInner>>,
    capacity: usize,
}

impl Default for RenderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct RenderRegistryInner {
    entries: HashMap<RenderKey, RenderEntry>,
    tick: u64,
}

struct RenderEntry {
    message_id: String,
    last_used: u64,
}

impl RenderRegistry {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_RENDER_REGISTRY_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(RenderRegistryInner::default())),
            capacity,
        }
    }

    pub fn anchor(&self, key: &RenderKey) -> Option<String> {
        let mut inner = self.inner.write();
        inner.tick = inner.tick.saturating_add(1);
        let last_used = inner.tick;
        let entry = inner.entries.get_mut(key)?;
        entry.last_used = last_used;
        Some(entry.message_id.clone())
    }

    pub fn set_anchor(&self, key: RenderKey, message_id: String) {
        let mut inner = self.inner.write();
        if self.capacity == 0 {
            inner.entries.clear();
            return;
        }

        inner.tick = inner.tick.saturating_add(1);
        let last_used = inner.tick;
        if let Some(entry) = inner.entries.get_mut(&key) {
            entry.message_id = message_id;
            entry.last_used = last_used;
            return;
        }

        if inner.entries.len() >= self.capacity {
            if let Some(victim) = inner
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
            {
                inner.entries.remove(&victim);
            }
        }

        inner.entries.insert(
            key,
            RenderEntry {
                message_id,
                last_used,
            },
        );
    }

    pub fn forget(&self, key: &RenderKey) {
        self.inner.write().entries.remove(key);
    }

    pub fn len(&self) -> usize {
        self.inner.read().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(platform: &str, chat: &str, turn: &str) -> RenderKey {
        RenderKey {
            platform: platform.into(),
            chat_id: chat.into(),
            turn_id: turn.into(),
        }
    }

    #[test]
    fn anchor_returns_none_for_unknown_key() {
        let r = RenderRegistry::new();
        assert!(r.anchor(&key("loopback", "c", "t1")).is_none());
    }

    #[test]
    fn set_anchor_then_anchor_returns_the_id() {
        let r = RenderRegistry::new();
        r.set_anchor(key("loopback", "c", "t1"), "msg-7".into());
        assert_eq!(
            r.anchor(&key("loopback", "c", "t1")).as_deref(),
            Some("msg-7")
        );
    }

    #[test]
    fn set_anchor_overwrites_existing_id() {
        let r = RenderRegistry::new();
        let k = key("loopback", "c", "t1");
        r.set_anchor(k.clone(), "msg-7".into());
        r.set_anchor(k.clone(), "msg-8".into());
        assert_eq!(r.anchor(&k).as_deref(), Some("msg-8"));
    }

    #[test]
    fn forget_removes_key() {
        let r = RenderRegistry::new();
        let k = key("loopback", "c", "t1");
        r.set_anchor(k.clone(), "msg-7".into());
        r.forget(&k);
        assert!(r.anchor(&k).is_none());
    }

    #[test]
    fn keys_with_different_turn_ids_are_distinct() {
        let r = RenderRegistry::new();
        r.set_anchor(key("loopback", "c", "t1"), "a".into());
        r.set_anchor(key("loopback", "c", "t2"), "b".into());
        assert_eq!(r.anchor(&key("loopback", "c", "t1")).as_deref(), Some("a"));
        assert_eq!(r.anchor(&key("loopback", "c", "t2")).as_deref(), Some("b"));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn registry_evicts_least_recently_used_entry_at_capacity() {
        let r = RenderRegistry::with_capacity(2);
        let first = key("loopback", "c", "t1");
        let second = key("loopback", "c", "t2");
        let third = key("loopback", "c", "t3");

        r.set_anchor(first.clone(), "a".into());
        r.set_anchor(second.clone(), "b".into());
        assert_eq!(r.anchor(&first).as_deref(), Some("a"), "read refreshes LRU");

        r.set_anchor(third.clone(), "c".into());

        assert_eq!(r.len(), 2, "registry must stay bounded by capacity");
        assert_eq!(r.anchor(&first).as_deref(), Some("a"));
        assert!(
            r.anchor(&second).is_none(),
            "least recently used entry evicted"
        );
        assert_eq!(r.anchor(&third).as_deref(), Some("c"));
    }
}
