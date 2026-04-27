//! In-memory map from (platform, chat_id, turn_id) → platform message id.
//!
//! StreamRenderer (PR-2a) writes the first chunk's anchor into this
//! registry once OutboundDispatcher captures it; subsequent chunks
//! read the anchor and emit OutboundMessages with `edit_target`
//! populated so the dispatcher routes to `Platform::edit`.
//!
//! Lifetime: entries live until the turn ends + a 5-minute grace
//! period. PR-2b (worker streaming switch) wires the explicit purge.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RenderKey {
    pub platform: String,
    pub chat_id: String,
    pub turn_id: String,
}

#[derive(Default)]
pub struct RenderRegistry {
    inner: Arc<RwLock<HashMap<RenderKey, String>>>,
}

impl RenderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn anchor(&self, key: &RenderKey) -> Option<String> {
        self.inner.read().get(key).cloned()
    }

    pub fn set_anchor(&self, key: RenderKey, message_id: String) {
        self.inner.write().insert(key, message_id);
    }

    pub fn forget(&self, key: &RenderKey) {
        self.inner.write().remove(key);
    }

    pub fn len(&self) -> usize {
        self.inner.read().len()
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
}
