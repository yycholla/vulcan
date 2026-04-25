use std::cell::Cell;

use chrono::Local;
use ratatui::style::Color;

use crate::hooks::audit::{AuditBuffer, AuditKind};

use super::theme::Palette;
use super::views::{DiffKind, DiffLine, View};

#[derive(Clone, Copy, Debug, Default)]
pub enum ChatRole {
    User,
    #[default]
    Agent,
    System,
}

#[derive(Clone, Debug, Default)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
    /// Reasoning trace for thinking-mode models (`Message::Assistant.reasoning_content`).
    /// Populated for agent messages from streaming `StreamEvent::Reasoning` deltas
    /// and from session-resume hydration. Empty for everything else.
    pub reasoning: String,
}

impl ChatMessage {
    pub fn new(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            reasoning: String::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionRow {
    pub name: String,
    pub status: String,
    pub tokens: String,
    pub color: Color,
    pub active: bool,
}

#[derive(Clone, Debug)]
pub struct SubAgentTile {
    pub name: String,
    pub role: String,
    pub state: String,
    pub color: Color,
    pub log: Vec<String>,
    pub cpu: Vec<u16>,
}

#[derive(Clone, Debug)]
pub struct ToolLogRow {
    pub time: String,
    pub actor: String,
    pub msg: String,
}

#[derive(Clone, Debug)]
pub struct TickerCell {
    pub sub: String,
    pub msg: String,
    pub color: Color,
}

#[derive(Clone, Debug)]
pub struct TreeNode {
    pub depth: u8,
    pub label: String,
    pub state: String,
    pub color: Color,
    pub active: bool,
}

pub struct AppState {
    pub view: View,
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub thinking: bool,
    pub scroll: u16,
    pub show_reasoning: bool,
    pub session_label: String,

    pub sessions: Vec<SessionRow>,
    pub subagents: Vec<SubAgentTile>,
    pub diff_lines: Vec<DiffLine>,
    pub ticker: Vec<TickerCell>,
    pub tree: Vec<TreeNode>,

    /// Optional shared handle to the audit-log hook's ring buffer. When
    /// present, the trading-floor tool-log pane renders from it; otherwise it
    /// falls back to demo data so the design still looks alive on first launch.
    pub audit_log: Option<AuditBuffer>,

    /// When the agent emits an `AgentPause`, the TUI parks it here. Render
    /// shows an overlay; key handler intercepts Y/A/N keys and consumes the
    /// pause via `take()` to send the reply.
    pub pending_pause: Option<crate::pause::AgentPause>,

    cursor: Cell<(u16, u16)>,
    pub model_label: String,
    pub token_used: u32,
    pub token_max: u32,
}

impl AppState {
    pub fn new(model_label: String, token_max: u32) -> Self {
        Self {
            view: View::TradingFloor,
            messages: Vec::new(),
            input: String::new(),
            thinking: false,
            scroll: 0,
            show_reasoning: true,
            session_label: "auth-refactor · 14:02".into(),

            sessions: demo_sessions(),
            subagents: demo_subagents(),
            diff_lines: demo_diff(),
            ticker: demo_ticker(),
            tree: demo_tree(),

            audit_log: None,
            pending_pause: None,

            cursor: Cell::new((0, 0)),
            model_label,
            token_used: 0,
            token_max,
        }
    }

    /// Snapshot the most recent N rows of the audit buffer for the
    /// trading-floor tool-log pane. Returns demo data if no audit buffer is
    /// attached or it's still empty.
    pub fn tool_log_view(&self, max: usize) -> Vec<ToolLogRow> {
        if let Some(buf) = &self.audit_log {
            if let Ok(buf) = buf.lock() {
                if !buf.is_empty() {
                    return buf
                        .iter()
                        .rev()
                        .take(max)
                        .map(|e| {
                            let kind_marker = match e.kind {
                                AuditKind::Started => "●",
                                AuditKind::Ok => "✓",
                                AuditKind::Err => "✗",
                            };
                            ToolLogRow {
                                time: e.time.with_timezone(&Local).format("%H:%M:%S").to_string(),
                                actor: short_tool(&e.tool),
                                msg: format!("{} {}", kind_marker, e.detail),
                            }
                        })
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                }
            }
        }
        demo_tool_log()
    }

    pub fn mode_label(&self) -> &'static str {
        if self.thinking { "RUN" } else { "INSERT" }
    }

    pub fn model_status(&self) -> String {
        let used_k = self.token_used / 1000;
        let max_k = self.token_max / 1000;
        format!("{} · {}k / {}k", self.model_label, used_k, max_k)
    }

    pub fn cursor_set(&self, x: u16, y: u16) {
        self.cursor.set((x, y));
    }

    pub fn cursor(&self) -> (u16, u16) {
        self.cursor.get()
    }
}

fn demo_sessions() -> Vec<SessionRow> {
    vec![
        SessionRow {
            name: "auth-refactor".into(),
            status: "live".into(),
            tokens: "18k".into(),
            color: Palette::RED,
            active: true,
        },
        SessionRow {
            name: "perf-investig.".into(),
            status: "wait".into(),
            tokens: "42k".into(),
            color: Palette::YELLOW,
            active: false,
        },
        SessionRow {
            name: "docs-rewrite".into(),
            status: "done".into(),
            tokens: "8k".into(),
            color: Palette::GREEN,
            active: false,
        },
        SessionRow {
            name: "fuzz-harness".into(),
            status: "live".into(),
            tokens: "64k".into(),
            color: Palette::BLUE,
            active: false,
        },
        SessionRow {
            name: "scratch".into(),
            status: "idle".into(),
            tokens: "2k".into(),
            color: Palette::MUTED,
            active: false,
        },
        SessionRow {
            name: "rfc-storage".into(),
            status: "idle".into(),
            tokens: "11k".into(),
            color: Palette::MUTED,
            active: false,
        },
    ]
}

fn demo_subagents() -> Vec<SubAgentTile> {
    vec![
        SubAgentTile {
            name: "main".into(),
            role: "orchestrator".into(),
            state: "thinking".into(),
            color: Palette::RED,
            log: vec![
                "delegating to 3 subs".into(),
                "watching token budget".into(),
                "merging diffs in 12s".into(),
            ],
            cpu: vec![2, 3, 4, 6, 4, 5, 7, 3],
        },
        SubAgentTile {
            name: "sub#a3f".into(),
            role: "users-svc".into(),
            state: "editing".into(),
            color: Palette::YELLOW,
            log: vec![
                "read auth.rs (847)".into(),
                "running cargo check".into(),
                "✓ tests pass".into(),
            ],
            cpu: vec![1, 3, 5, 4, 6, 7, 4, 5],
        },
        SubAgentTile {
            name: "sub#b21".into(),
            role: "billing-svc".into(),
            state: "running".into(),
            color: Palette::BLUE,
            log: vec![
                "grep AuthMiddleware".into(),
                "found 6 callsites".into(),
                "patching trait impl…".into(),
            ],
            cpu: vec![2, 4, 3, 5, 7, 8, 6, 9],
        },
        SubAgentTile {
            name: "sub#c08".into(),
            role: "gateway".into(),
            state: "blocked".into(),
            color: Palette::MUTED,
            log: vec!["waiting on a3f".into(), "queue: 1 task".into(), "—".into()],
            cpu: vec![1, 1, 1, 2, 1, 1, 2, 1],
        },
    ]
}

/// Compress a long tool name for the actor column (5 chars wide).
fn short_tool(name: &str) -> String {
    if name.len() <= 5 {
        return name.to_string();
    }
    name.chars().take(5).collect()
}

fn demo_tool_log() -> Vec<ToolLogRow> {
    vec![
        ToolLogRow { time: "14:02:13".into(), actor: "main".into(), msg: "spawn ×3".into() },
        ToolLogRow { time: "14:02:15".into(), actor: "a3f".into(), msg: "read_file ✓".into() },
        ToolLogRow { time: "14:02:18".into(), actor: "b21".into(), msg: "grep ✓ 14m".into() },
        ToolLogRow { time: "14:02:22".into(), actor: "a3f".into(), msg: "edit ✓ 12h".into() },
        ToolLogRow { time: "14:02:25".into(), actor: "b21".into(), msg: "edit ●".into() },
        ToolLogRow { time: "14:02:29".into(), actor: "a3f".into(), msg: "cargo ✓".into() },
        ToolLogRow { time: "14:02:31".into(), actor: "main".into(), msg: "merge ?".into() },
    ]
}

fn demo_diff() -> Vec<DiffLine> {
    vec![
        DiffLine { text: "@@ -88,6 +88,8 @@".into(), kind: DiffKind::Hunk },
        DiffLine {
            text: "- pub fn verify(&self, t: &Token) {".into(),
            kind: DiffKind::Removed,
        },
        DiffLine {
            text: "+ pub fn verify<'a>(&self, t: &'a Token) -> Result<Claims> {".into(),
            kind: DiffKind::Added,
        },
        DiffLine { text: "    let claims = decode(t)?;".into(), kind: DiffKind::Ctx },
        DiffLine { text: "+   self.audit.log(&claims);".into(), kind: DiffKind::Added },
        DiffLine { text: "    Ok(claims)".into(), kind: DiffKind::Ctx },
    ]
}

fn demo_ticker() -> Vec<TickerCell> {
    let subs = ["a3f", "b21", "c08", "d9k", "e44", "f12"];
    let msgs = ["✓ tests", "edit", "grep", "run", "wait", "diff", "merge", "fork"];
    let cols = [Palette::GREEN, Palette::YELLOW, Palette::BLUE, Palette::RED];
    (0..16)
        .map(|i| TickerCell {
            sub: subs[i % subs.len()].into(),
            msg: msgs[i % msgs.len()].into(),
            color: cols[i % cols.len()],
        })
        .collect()
}

fn demo_tree() -> Vec<TreeNode> {
    vec![
        TreeNode { depth: 0, label: "main · auth-refactor".into(), state: "★".into(), color: Palette::RED, active: false },
        TreeNode { depth: 1, label: "├─ plan A: bottom-up".into(), state: "✓".into(), color: Palette::GREEN, active: false },
        TreeNode { depth: 2, label: "│  ├─ users-svc".into(), state: "✓".into(), color: Palette::GREEN, active: false },
        TreeNode { depth: 2, label: "│  ├─ billing-svc".into(), state: "●".into(), color: Palette::YELLOW, active: true },
        TreeNode { depth: 2, label: "│  └─ gateway".into(), state: "○".into(), color: Palette::MUTED, active: false },
        TreeNode { depth: 1, label: "├─ plan B: feature-flag".into(), state: "✗".into(), color: Palette::RED, active: false },
        TreeNode { depth: 2, label: "│  └─ rejected: 2× cost".into(), state: " ".into(), color: Palette::MUTED, active: false },
        TreeNode { depth: 1, label: "└─ plan C: rewrite (sub#x)".into(), state: "?".into(), color: Palette::BLUE, active: false },
        TreeNode { depth: 2, label: "   └─ exploring…".into(), state: "●".into(), color: Palette::YELLOW, active: false },
    ]
}
