//! Reference hook: in-memory audit log of every tool call.
//!
//! The same `Arc<Mutex<VecDeque<AuditEntry>>>` is shared with the TUI so the
//! trading-floor "tool log" pane renders real activity rather than mock data.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::tools::ToolResult;

use super::{HookHandler, HookOutcome};

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

pub struct AuditHook {
    buf: AuditBuffer,
    capacity: usize,
}

impl AuditHook {
    /// Returns the hook plus a shared handle to the buffer so the TUI (or any
    /// other consumer) can read it.
    pub fn new(capacity: usize) -> (Arc<Self>, AuditBuffer) {
        let buf: AuditBuffer = Arc::new(Mutex::new(VecDeque::with_capacity(capacity)));
        let hook = Arc::new(Self {
            buf: buf.clone(),
            capacity,
        });
        (hook, buf)
    }

    fn push(&self, entry: AuditEntry) {
        if let Ok(mut buf) = self.buf.lock() {
            if buf.len() >= self.capacity {
                buf.pop_front();
            }
            buf.push_back(entry);
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

    async fn before_tool_call(&self, tool: &str, args: &Value) -> Result<HookOutcome> {
        let detail = args.to_string();
        self.push(AuditEntry {
            time: Utc::now(),
            kind: AuditKind::Started,
            tool: tool.to_string(),
            detail: truncate(&detail, 80),
        });
        Ok(HookOutcome::Continue)
    }

    async fn after_tool_call(&self, tool: &str, result: &ToolResult) -> Result<HookOutcome> {
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
