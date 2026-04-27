//! Reference hook: in-memory audit log of every tool call.
//!
//! The same `Arc<Mutex<VecDeque<AuditEntry>>>` is shared with the TUI so the
//! trading-floor "tool log" pane renders real activity rather than mock data.
//!
//! YYC-88 also gives this hook a side counter that tracks bash invocations
//! that match a native-tool redirect (per `prefer_native::match_native_category`).
//! The TUI surfaces a "bash misuse: N redirects (rg=2, cargo=1)" line at
//! session end so we can see whether the YYC-84 nudges actually move the
//! needle.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::hooks::prefer_native::match_native_category;
use crate::tools::ToolResult;

use super::{HookHandler, HookOutcome};

/// Telemetry slice tracking how often the agent reaches for `bash` when a
/// native tool exists. `legitimate` counts bash calls that fell through
/// the redirect matcher (pipes, multi-stage shell, unrelated commands) —
/// they're not mistakes, they're the genuine bash-only jobs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BashRedirectCounts {
    pub total: u32,
    pub legitimate: u32,
    pub by_category: BTreeMap<String, u32>,
}

impl BashRedirectCounts {
    pub fn redirected(&self) -> u32 {
        self.by_category.values().sum()
    }

    /// Render the one-line summary the TUI shows at session end. Empty
    /// when no bash calls were observed at all.
    pub fn summary_line(&self) -> Option<String> {
        if self.total == 0 {
            return None;
        }
        let redirected = self.redirected();
        if redirected == 0 {
            return Some(format!(
                "bash misuse: 0 redirects, {} legitimate",
                self.legitimate
            ));
        }
        let parts: Vec<String> = self
            .by_category
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        Some(format!(
            "bash misuse: {} redirects ({}), {} legitimate",
            redirected,
            parts.join(", "),
            self.legitimate
        ))
    }
}

#[derive(Clone, Debug)]
pub enum AuditKind {
    Started,
    Ok,
    Err,
}

#[derive(Clone, Debug)]
pub struct AuditEntry {
    pub time: DateTime<Utc>,
    pub kind: AuditKind,
    pub tool: String,
    pub detail: String,
}

pub type AuditBuffer = Arc<Mutex<VecDeque<AuditEntry>>>;
pub type BashCountsHandle = Arc<Mutex<BashRedirectCounts>>;

pub struct AuditHook {
    buf: AuditBuffer,
    capacity: usize,
    bash_counts: BashCountsHandle,
}

impl AuditHook {
    /// Returns the hook plus a shared handle to the buffer so the TUI (or any
    /// other consumer) can read it.
    pub fn new(capacity: usize) -> (Arc<Self>, AuditBuffer) {
        let (hook, buf, _) = Self::with_bash_counters(capacity);
        (hook, buf)
    }

    /// Same as `new`, plus a handle to the YYC-88 bash-redirect counter
    /// so the TUI / session-end summary can read aggregated misuse stats
    /// without reaching back through `Agent`.
    pub fn with_bash_counters(capacity: usize) -> (Arc<Self>, AuditBuffer, BashCountsHandle) {
        let buf: AuditBuffer = Arc::new(Mutex::new(VecDeque::with_capacity(capacity)));
        let bash_counts: BashCountsHandle = Arc::new(Mutex::new(BashRedirectCounts::default()));
        let hook = Arc::new(Self {
            buf: buf.clone(),
            capacity,
            bash_counts: bash_counts.clone(),
        });
        (hook, buf, bash_counts)
    }

    /// Snapshot the current bash-redirect counts. Useful in tests; the
    /// TUI consumes the live handle directly.
    pub fn bash_counts_snapshot(&self) -> BashRedirectCounts {
        self.bash_counts
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default()
    }

    fn push(&self, entry: AuditEntry) {
        if let Ok(mut buf) = self.buf.lock() {
            if buf.len() >= self.capacity {
                buf.pop_front();
            }
            buf.push_back(entry);
        }
    }

    fn record_bash(&self, command: &str) {
        let category = match_native_category(command);
        if let Ok(mut counts) = self.bash_counts.lock() {
            counts.total = counts.total.saturating_add(1);
            match category {
                Some(cat) => {
                    *counts.by_category.entry(cat.to_string()).or_insert(0) += 1;
                }
                None => {
                    counts.legitimate = counts.legitimate.saturating_add(1);
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl HookHandler for AuditHook {
    fn name(&self) -> &str {
        "audit-log"
    }

    // Run as early as possible so the audit log records what the tool actually
    // received, before any other hook mutates args.
    fn priority(&self) -> i32 {
        1
    }

    async fn before_tool_call(
        &self,
        tool: &str,
        args: &Value,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        let detail = args.to_string();
        self.push(AuditEntry {
            time: Utc::now(),
            kind: AuditKind::Started,
            tool: tool.to_string(),
            detail: truncate(&detail, 80),
        });
        // YYC-88: count every bash invocation, classifying by whether
        // it would have been redirected to a native tool.
        if tool == "bash"
            && let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                self.record_bash(cmd);
            }
        Ok(HookOutcome::Continue)
    }

    async fn after_tool_call(
        &self,
        tool: &str,
        result: &ToolResult,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        let kind = if result.is_error {
            AuditKind::Err
        } else {
            AuditKind::Ok
        };
        self.push(AuditEntry {
            time: Utc::now(),
            kind,
            tool: tool.to_string(),
            detail: truncate(&first_line(&result.output), 80),
        });
        Ok(HookOutcome::Continue)
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn run_bash(hook: &AuditHook, cmd: &str) {
        let args = json!({ "command": cmd });
        let _ = hook
            .before_tool_call("bash", &args, CancellationToken::new())
            .await;
    }

    #[tokio::test]
    async fn bash_redirect_counter_classifies_each_call() {
        let (hook, _buf, counts) = AuditHook::with_bash_counters(64);

        run_bash(&hook, "rg foo").await;
        run_bash(&hook, "cargo build").await;
        run_bash(&hook, "find . -name '*.rs'").await;
        run_bash(&hook, "rg bar src/").await;
        // Multi-stage shell — should land in `legitimate`.
        run_bash(&hook, "rg foo | head -3").await;
        // Unrelated command — also legitimate.
        run_bash(&hook, "npm install").await;

        let snap = counts.lock().unwrap().clone();
        assert_eq!(snap.total, 6);
        assert_eq!(snap.redirected(), 4);
        assert_eq!(snap.legitimate, 2);
        assert_eq!(snap.by_category.get("rg").copied(), Some(2));
        assert_eq!(snap.by_category.get("cargo").copied(), Some(1));
        assert_eq!(snap.by_category.get("find").copied(), Some(1));
    }

    #[tokio::test]
    async fn non_bash_tools_dont_touch_redirect_counter() {
        let (hook, _buf, counts) = AuditHook::with_bash_counters(64);
        let _ = hook
            .before_tool_call(
                "read_file",
                &json!({"path": "/tmp/x"}),
                CancellationToken::new(),
            )
            .await;
        let snap = counts.lock().unwrap().clone();
        assert_eq!(snap, BashRedirectCounts::default());
    }

    #[test]
    fn summary_line_groups_categories_or_says_nothing_when_idle() {
        let mut c = BashRedirectCounts::default();
        assert_eq!(c.summary_line(), None);

        c.total = 3;
        c.legitimate = 2;
        *c.by_category.entry("rg".into()).or_insert(0) += 1;
        let s = c.summary_line().unwrap();
        assert!(s.contains("1 redirect"), "got {s:?}");
        assert!(s.contains("rg=1"), "got {s:?}");
        assert!(s.contains("2 legitimate"), "got {s:?}");

        let only_legit = BashRedirectCounts {
            total: 2,
            legitimate: 2,
            by_category: BTreeMap::new(),
        };
        assert!(
            only_legit
                .summary_line()
                .unwrap()
                .contains("0 redirects, 2 legitimate")
        );
    }

    #[test]
    fn snapshot_returns_default_before_any_calls() {
        let (hook, _buf, _counts) = AuditHook::with_bash_counters(64);
        assert_eq!(hook.bash_counts_snapshot(), BashRedirectCounts::default());
    }
}
