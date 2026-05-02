//! YYC-237 (YYC-169 PR-2): extension audit stream + quota
//! tracker.
//!
//! Bounded in-memory ring of `ExtensionAuditEvent`s plus a
//! `QuotaTracker` keyed by `(extension_id, permission)`. Both
//! are pure data — PR-3 wires them into the tool-dispatch path.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::hooks::RewriteRejection;

use super::policy::{ExtensionPermission, PolicyDecision};

/// Permission-keyed audit row. Original YYC-237 shape; covers
/// filesystem / network / shell decisions emitted by the tool
/// dispatch path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionAuditEvent {
    pub extension_id: String,
    pub permission: ExtensionPermission,
    pub decision: PolicyDecision,
    /// Free-form target description ("read /etc/hosts",
    /// "POST https://...", "spawn rg"). Optional — short-form
    /// requests may omit it.
    pub target: Option<String>,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
}

/// GH issue #557: action an `on_input` extension hook took on user
/// input. `Block` short-circuits with the supplied reason; `Replace`
/// substitutes the `after` text from the parent event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum InputInterceptAction {
    Block { reason: String },
    Replace,
}

/// GH issue #557: input-intercept audit row. Recorded whenever an
/// extension's `on_input` hook returns a non-Continue outcome.
/// `before` is the raw user input; `after` is what the daemon
/// dispatched (the rewrite, or the original on Block).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputInterceptEvent {
    pub extension_id: String,
    pub before: String,
    pub after: String,
    pub action: InputInterceptAction,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum CompactionAuditAction {
    ValidationFailed { rejection: RewriteRejection },
    Forced { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionAuditEvent {
    pub extension_id: String,
    pub action: CompactionAuditAction,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
}

/// Polymorphic audit row recorded into [`ExtensionAuditLog`]. Each
/// variant carries the typed payload for one event class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ExtensionAuditEvent {
    Permission(PermissionAuditEvent),
    InputIntercept(InputInterceptEvent),
    Compaction(CompactionAuditEvent),
}

impl ExtensionAuditEvent {
    /// Common accessor across variants — every audit row knows which
    /// extension produced it.
    pub fn extension_id(&self) -> &str {
        match self {
            Self::Permission(p) => &p.extension_id,
            Self::InputIntercept(i) => &i.extension_id,
            Self::Compaction(c) => &c.extension_id,
        }
    }

    /// Common accessor across variants — every audit row carries a
    /// timestamp.
    pub fn occurred_at(&self) -> chrono::DateTime<chrono::Utc> {
        match self {
            Self::Permission(p) => p.occurred_at,
            Self::InputIntercept(i) => i.occurred_at,
            Self::Compaction(c) => c.occurred_at,
        }
    }
}

#[derive(Debug)]
pub struct ExtensionAuditLog {
    inner: Mutex<Vec<ExtensionAuditEvent>>,
    cap: usize,
}

impl Default for ExtensionAuditLog {
    fn default() -> Self {
        Self::new(512)
    }
}

impl ExtensionAuditLog {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Mutex::new(Vec::new()),
            cap: cap.max(1),
        }
    }

    pub fn record(&self, event: ExtensionAuditEvent) {
        let mut guard = self.inner.lock();
        if guard.len() >= self.cap {
            guard.remove(0);
        }
        guard.push(event);
    }

    pub fn recent(&self, limit: usize) -> Vec<ExtensionAuditEvent> {
        let guard = self.inner.lock();
        guard.iter().rev().take(limit).cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }
}

/// Per-extension permission counter. `record` increments the
/// counter and `would_exceed` reports whether the current count
/// has hit `limit` already (use before allowing the next call).
#[derive(Debug, Default)]
pub struct QuotaTracker {
    inner: Mutex<HashMap<(String, ExtensionPermission), u32>>,
}

impl QuotaTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, extension_id: &str, permission: ExtensionPermission) -> u32 {
        let mut guard = self.inner.lock();
        let key = (extension_id.to_string(), permission);
        let counter = guard.entry(key).or_insert(0);
        *counter = counter.saturating_add(1);
        *counter
    }

    pub fn count(&self, extension_id: &str, permission: ExtensionPermission) -> u32 {
        self.inner
            .lock()
            .get(&(extension_id.to_string(), permission))
            .copied()
            .unwrap_or(0)
    }

    /// Returns `true` when the next call would push the counter
    /// past `limit`. Use as the gate before `record`.
    pub fn would_exceed(
        &self,
        extension_id: &str,
        permission: ExtensionPermission,
        limit: u32,
    ) -> bool {
        self.count(extension_id, permission) >= limit
    }

    pub fn reset(&self, extension_id: &str, permission: ExtensionPermission) {
        self.inner
            .lock()
            .remove(&(extension_id.to_string(), permission));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_event(id: &str) -> ExtensionAuditEvent {
        ExtensionAuditEvent::Permission(PermissionAuditEvent {
            extension_id: id.to_string(),
            permission: ExtensionPermission::FilesystemRead,
            decision: PolicyDecision::Allow,
            target: Some("/etc/hosts".into()),
            occurred_at: chrono::Utc::now(),
        })
    }

    #[test]
    fn audit_log_records_and_returns_recent_first() {
        let log = ExtensionAuditLog::new(8);
        log.record(fixture_event("a"));
        log.record(fixture_event("b"));
        log.record(fixture_event("c"));
        let recent = log.recent(2);
        let ids: Vec<&str> = recent.iter().map(|e| e.extension_id()).collect();
        assert_eq!(ids, vec!["c", "b"]);
    }

    #[test]
    fn audit_log_evicts_oldest_when_cap_reached() {
        let log = ExtensionAuditLog::new(2);
        log.record(fixture_event("a"));
        log.record(fixture_event("b"));
        log.record(fixture_event("c"));
        assert_eq!(log.len(), 2);
        let recent = log.recent(10);
        let ids: Vec<&str> = recent.iter().map(|e| e.extension_id()).collect();
        assert_eq!(ids, vec!["c", "b"]);
    }

    #[test]
    fn quota_tracker_increments_and_resets() {
        let q = QuotaTracker::new();
        q.record("alpha", ExtensionPermission::Network);
        q.record("alpha", ExtensionPermission::Network);
        q.record("beta", ExtensionPermission::Network);
        assert_eq!(q.count("alpha", ExtensionPermission::Network), 2);
        assert_eq!(q.count("beta", ExtensionPermission::Network), 1);
        q.reset("alpha", ExtensionPermission::Network);
        assert_eq!(q.count("alpha", ExtensionPermission::Network), 0);
    }

    #[test]
    fn would_exceed_reports_at_threshold() {
        let q = QuotaTracker::new();
        q.record("alpha", ExtensionPermission::Shell);
        q.record("alpha", ExtensionPermission::Shell);
        assert!(!q.would_exceed("alpha", ExtensionPermission::Shell, 5));
        assert!(q.would_exceed("alpha", ExtensionPermission::Shell, 2));
        assert!(q.would_exceed("alpha", ExtensionPermission::Shell, 1));
    }

    #[test]
    fn audit_event_round_trips_through_serde_json() {
        let evt = fixture_event("zeta");
        let json = serde_json::to_string(&evt).unwrap();
        let back: ExtensionAuditEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn audit_log_records_input_intercept_event_and_returns_it() {
        let log = ExtensionAuditLog::new(8);
        let evt = ExtensionAuditEvent::InputIntercept(InputInterceptEvent {
            extension_id: "input-demo".to_string(),
            before: "hi".to_string(),
            after: "hello".to_string(),
            action: InputInterceptAction::Replace,
            occurred_at: chrono::Utc::now(),
        });
        log.record(evt.clone());
        let recent = log.recent(1);
        assert_eq!(recent.len(), 1);
        match &recent[0] {
            ExtensionAuditEvent::InputIntercept(intercept) => {
                assert_eq!(intercept.before, "hi");
                assert_eq!(intercept.after, "hello");
                assert!(matches!(intercept.action, InputInterceptAction::Replace));
            }
            other => panic!("expected InputIntercept, got {other:?}"),
        }
    }

    #[test]
    fn input_intercept_event_round_trips_through_serde_json() {
        let evt = ExtensionAuditEvent::InputIntercept(InputInterceptEvent {
            extension_id: "input-demo".to_string(),
            before: "raw".to_string(),
            after: "expanded".to_string(),
            action: InputInterceptAction::Block {
                reason: "policy".into(),
            },
            occurred_at: chrono::Utc::now(),
        });
        let json = serde_json::to_string(&evt).unwrap();
        let back: ExtensionAuditEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
    }

    #[test]
    fn audit_log_records_compaction_validation_failed_event() {
        let log = ExtensionAuditLog::new(8);
        let evt = ExtensionAuditEvent::Compaction(CompactionAuditEvent {
            extension_id: "compact-summary".to_string(),
            action: CompactionAuditAction::ValidationFailed {
                rejection: RewriteRejection::MissingSystem,
            },
            occurred_at: chrono::Utc::now(),
        });
        log.record(evt.clone());

        assert_eq!(log.recent(1), vec![evt]);
    }

    #[test]
    fn compaction_forced_event_round_trips_through_serde_json() {
        let evt = ExtensionAuditEvent::Compaction(CompactionAuditEvent {
            extension_id: "compact-summary".to_string(),
            action: CompactionAuditAction::Forced {
                reason: "overflow".to_string(),
            },
            occurred_at: chrono::Utc::now(),
        });

        let json = serde_json::to_string(&evt).unwrap();
        assert!(json.contains("compaction"));
        assert!(json.contains("forced"));
        let back: ExtensionAuditEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, evt);
    }
}
