use std::cell::Cell;

use chrono::Local;
use ratatui::style::Color;

use crate::hooks::audit::{AuditBuffer, AuditKind};
use crate::memory::SessionSummary;

use super::theme::Palette;
use super::views::{DiffKind, DiffLine, View};

/// Prompt-row state machine (YYC-58). Drives the mode badge and the
/// per-mode key dispatch + hint set. `Busy` is a transient state pinned
/// while the agent is mid-turn (YYC-61); it lives on this enum so the
/// queue path can use one classification rather than a parallel flag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PromptMode {
    /// Default text entry.
    #[default]
    Insert,
    /// User is typing a slash command.
    Command,
    /// Agent has paused for a user response (AgentPause).
    Ask,
    /// Agent is mid-turn; characters still type but Enter enqueues
    /// instead of sending (YYC-61).
    Busy,
}

impl PromptMode {
    /// Short uppercase badge shown in the prompt row's mode pill.
    pub fn badge(self) -> &'static str {
        match self {
            PromptMode::Insert => "INSERT",
            PromptMode::Command => "CMD",
            PromptMode::Ask => "ASK",
            PromptMode::Busy => "BUSY",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub enum ChatRole {
    User,
    #[default]
    Agent,
    System,
}

/// In-flight or completed tool call rendered inside an agent message timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    InProgress,
    /// `bool` is the success flag — true → ✓, false → ✗.
    Done(bool),
}

/// One slice of an agent's response timeline, in arrival order. Segments
/// preserve the natural interleaving of reasoning, tool calls, and text so
/// the renderer can show the agent's actual flow (think → tool → think →
/// answer) instead of bunching all reasoning above all tool calls (YYC-71).
#[derive(Clone, Debug)]
pub enum MessageSegment {
    Reasoning(String),
    Text(String),
    ToolCall { name: String, status: ToolStatus },
}

#[derive(Clone, Debug, Default)]
pub struct ChatMessage {
    pub role: ChatRole,
    /// Flattened text content (kept for hydration from `Message::Assistant`
    /// where the wire format only has aggregate content + reasoning, no
    /// per-event timeline). Used as a fallback by the renderer when
    /// `segments` is empty.
    pub content: String,
    /// Aggregate reasoning trace for hydrated messages. Renderer falls back
    /// to this only when `segments` is empty.
    pub reasoning: String,
    /// Ordered timeline of reasoning fragments, tool calls, and text chunks
    /// as they arrived. Populated for live streamed agent messages; empty
    /// for user/system messages and hydrated history.
    pub segments: Vec<MessageSegment>,
}

impl ChatMessage {
    pub fn new(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            reasoning: String::new(),
            segments: Vec::new(),
        }
    }

    /// True if neither the live segment timeline nor the hydrated content
    /// field has any text. Used by the renderer to decide whether to show
    /// the streaming "Thinking…" / "Answering…" placeholder.
    pub fn has_text(&self) -> bool {
        if !self.content.is_empty() {
            return true;
        }
        self.segments
            .iter()
            .any(|s| matches!(s, MessageSegment::Text(t) if !t.is_empty()))
    }

    /// True if any reasoning has been recorded — either streamed into
    /// segments or hydrated into the legacy `reasoning` field.
    pub fn has_reasoning(&self) -> bool {
        if !self.reasoning.is_empty() {
            return true;
        }
        self.segments
            .iter()
            .any(|s| matches!(s, MessageSegment::Reasoning(r) if !r.is_empty()))
    }

    /// Append a text chunk to the segment timeline, coalescing with the
    /// trailing segment if it's also text.
    pub fn append_text(&mut self, chunk: &str) {
        match self.segments.last_mut() {
            Some(MessageSegment::Text(t)) => t.push_str(chunk),
            _ => self.segments.push(MessageSegment::Text(chunk.to_string())),
        }
    }

    /// Append a reasoning chunk to the segment timeline, coalescing with the
    /// trailing segment if it's also reasoning. New tool calls or text break
    /// the run, so subsequent reasoning starts a fresh segment — that's the
    /// whole point of YYC-71.
    pub fn append_reasoning(&mut self, chunk: &str) {
        match self.segments.last_mut() {
            Some(MessageSegment::Reasoning(r)) => r.push_str(chunk),
            _ => self
                .segments
                .push(MessageSegment::Reasoning(chunk.to_string())),
        }
    }

    pub fn push_tool_start(&mut self, name: impl Into<String>) {
        self.segments.push(MessageSegment::ToolCall {
            name: name.into(),
            status: ToolStatus::InProgress,
        });
    }

    /// Mark the most recent in-progress ToolCall with this name as done.
    /// Walks segments in reverse so concurrent dispatch (YYC-34) still
    /// pairs each end with its own start as the matching tail.
    pub fn finish_tool(&mut self, name: &str, ok: bool) {
        for seg in self.segments.iter_mut().rev() {
            if let MessageSegment::ToolCall {
                name: n,
                status: status @ ToolStatus::InProgress,
            } = seg
            {
                if n == name {
                    *status = ToolStatus::Done(ok);
                    return;
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionState {
    pub id: String,
    pub label: String,
    pub message_count: usize,
    pub created_at: i64,
    pub last_active: i64,
    pub parent_session_id: Option<String>,
    pub lineage_label: Option<String>,
    pub status: SessionStatus,
    pub is_active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Live,
    Saved,
}

impl SessionStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Saved => "saved",
        }
    }
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OrchestrationPhase {
    #[default]
    Idle,
    Thinking,
    ToolRunning,
    Paused,
    Complete,
    Error,
}

impl OrchestrationPhase {
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Thinking => "thinking",
            Self::ToolRunning => "tool",
            Self::Paused => "paused",
            Self::Complete => "done",
            Self::Error => "error",
        }
    }

    fn color(self) -> Color {
        match self {
            Self::Idle => Palette::MUTED,
            Self::Thinking => Palette::YELLOW,
            Self::ToolRunning => Palette::BLUE,
            Self::Paused => Palette::RED,
            Self::Complete => Palette::GREEN,
            Self::Error => Palette::RED,
        }
    }

    fn symbol(self) -> &'static str {
        match self {
            Self::Idle => "○",
            Self::Thinking => "●",
            Self::ToolRunning => "●",
            Self::Paused => "⏸",
            Self::Complete => "✓",
            Self::Error => "✗",
        }
    }
}

#[derive(Clone, Debug)]
pub struct OrchestrationEvent {
    pub actor: String,
    pub msg: String,
    pub color: Color,
}

#[derive(Clone, Debug)]
pub struct OrchestrationState {
    pub phase: OrchestrationPhase,
    pub active_task: String,
    pub current_tool: Option<String>,
    pub recent_events: Vec<OrchestrationEvent>,
}

impl Default for OrchestrationState {
    fn default() -> Self {
        Self {
            phase: OrchestrationPhase::Idle,
            active_task: "Awaiting user input".into(),
            current_tool: None,
            recent_events: Vec::new(),
        }
    }
}

pub struct AppState {
    pub view: View,
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub thinking: bool,
    pub scroll: u16,
    /// True when the chat viewport is following the bottom — new agent
    /// content auto-scrolls into view. Set false the moment the user scrolls
    /// up; flipped back true when they scroll all the way back down or
    /// submit a new prompt (YYC-69).
    pub at_bottom: bool,
    /// Last computed max scroll position for the chat viewport, written by
    /// the renderer on each frame. The main loop reads this to keep
    /// `scroll` pinned to the bottom while `at_bottom` is true.
    pub chat_max_scroll: Cell<u16>,
    /// Highlighted row in the slash command palette (YYC-70). Reset to 0
    /// whenever the filter changes; navigated via arrow keys or Ctrl+J/K.
    pub slash_menu_selection: usize,
    /// Prompt-row mode (YYC-58). Drives the mode badge, the per-mode
    /// hint set, and which key bindings the dispatcher honors.
    pub prompt_mode: PromptMode,
    pub show_reasoning: bool,
    pub session_label: String,

    pub sessions: Vec<SessionState>,
    pub active_session_id: Option<String>,
    pub diff_lines: Vec<DiffLine>,
    pub orchestration: OrchestrationState,

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
            at_bottom: true,
            chat_max_scroll: Cell::new(0),
            slash_menu_selection: 0,
            prompt_mode: PromptMode::Insert,
            show_reasoning: true,
            session_label: "new session".into(),

            sessions: Vec::new(),
            active_session_id: None,
            diff_lines: demo_diff(),
            orchestration: OrchestrationState::default(),

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
        // YYC-58: badge follows the prompt mode rather than thinking flag.
        // Busy is set externally when a turn starts (YYC-61).
        self.prompt_mode.badge()
    }

    /// Per-mode hint pairs for the prompt-row footer (YYC-58). Centralized
    /// so call sites in views don't drift apart on the bindings each mode
    /// advertises.
    pub fn prompt_hints(&self) -> &'static [(&'static str, &'static str)] {
        match self.prompt_mode {
            PromptMode::Insert => &[
                ("↵", "send"),
                ("⌃T", "tools"),
                ("⌃K", "sessions"),
                ("/", "cmds"),
            ],
            PromptMode::Command => &[
                ("↵", "run"),
                ("↑↓", "select"),
                ("Tab", "complete"),
                ("Esc", "cancel"),
            ],
            PromptMode::Ask => &[("y", "proceed"), ("n", "deny"), ("Esc", "cancel")],
            PromptMode::Busy => &[
                ("↵", "queue"),
                ("⌃C", "cancel"),
                ("⌃⌫", "drop last"),
            ],
        }
    }

    /// True while an agent turn is in flight. Backed by `thinking` for now;
    /// kept as a method so callers (queue, dispatcher) don't reach into the
    /// flag directly (YYC-61, YYC-62).
    pub fn is_busy(&self) -> bool {
        self.thinking
    }

    /// Recompute `prompt_mode` from observable state (pending pause,
    /// thinking flag, input prefix). Called once per loop tick before
    /// draw so the badge tracks reality without each call site updating
    /// it manually.
    pub fn refresh_prompt_mode(&mut self) {
        self.prompt_mode = if self.pending_pause.is_some() {
            PromptMode::Ask
        } else if self.thinking {
            PromptMode::Busy
        } else if self.input.starts_with('/') {
            PromptMode::Command
        } else {
            PromptMode::Insert
        };
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

    pub fn hydrate_sessions(&mut self, summaries: &[SessionSummary], active_session_id: &str) {
        self.active_session_id = Some(active_session_id.to_string());
        self.sessions = summaries
            .iter()
            .map(|s| {
                let is_active = s.id == active_session_id;
                SessionState {
                    id: s.id.clone(),
                    label: short_session_id(&s.id),
                    message_count: s.message_count,
                    created_at: s.created_at,
                    last_active: s.last_active,
                    parent_session_id: s.parent_session_id.clone(),
                    lineage_label: s.lineage_label.clone(),
                    status: if is_active {
                        SessionStatus::Live
                    } else {
                        SessionStatus::Saved
                    },
                    is_active,
                }
            })
            .collect();
        self.session_label = self
            .active_session()
            .map(|s| s.label.clone())
            .unwrap_or_else(|| short_session_id(active_session_id));
    }

    pub fn note_prompt_submitted(&mut self, prompt: &str) {
        self.orchestration.phase = OrchestrationPhase::Thinking;
        self.orchestration.current_tool = None;
        self.orchestration.active_task = format!("Answering: {}", short_text(prompt, 56));
        self.push_event(
            "main",
            format!("received prompt: {}", short_text(prompt, 36)),
            Palette::RED,
        );
    }

    pub fn note_reasoning(&mut self) {
        if self.orchestration.phase != OrchestrationPhase::ToolRunning {
            self.orchestration.phase = OrchestrationPhase::Thinking;
        }
        if self.orchestration.active_task == "Awaiting user input" {
            self.orchestration.active_task = "Reasoning about the current turn".into();
        }
    }

    pub fn note_tool_start(&mut self, name: &str) {
        self.orchestration.phase = OrchestrationPhase::ToolRunning;
        self.orchestration.current_tool = Some(name.to_string());
        self.orchestration.active_task = format!("Running tool `{name}`");
        self.push_event("main", format!("started tool `{name}`"), Palette::BLUE);
    }

    pub fn note_tool_end(&mut self, name: &str, ok: bool) {
        self.orchestration.phase = if ok {
            OrchestrationPhase::Thinking
        } else {
            OrchestrationPhase::Error
        };
        self.orchestration.current_tool = None;
        self.orchestration.active_task = if ok {
            format!("Tool `{name}` completed; continuing turn")
        } else {
            format!("Tool `{name}` failed")
        };
        let color = if ok { Palette::GREEN } else { Palette::RED };
        let status = if ok { "completed" } else { "failed" };
        self.push_event("main", format!("tool `{name}` {status}"), color);
    }

    pub fn note_pause(&mut self, summary: &str) {
        self.orchestration.phase = OrchestrationPhase::Paused;
        self.orchestration.active_task = short_text(summary, 64);
        self.push_event("main", "waiting for user approval".into(), Palette::RED);
    }

    pub fn note_resume(&mut self, label: &str) {
        self.orchestration.phase = OrchestrationPhase::Thinking;
        self.orchestration.active_task = format!("Resumed: {label}");
        self.push_event("main", format!("resumed — {label}"), Palette::GREEN);
    }

    pub fn note_done(&mut self) {
        self.orchestration.phase = OrchestrationPhase::Complete;
        self.orchestration.current_tool = None;
        self.orchestration.active_task = "Turn complete".into();
        self.push_event("main", "completed turn".into(), Palette::GREEN);
    }

    pub fn note_error(&mut self, msg: &str) {
        self.orchestration.phase = OrchestrationPhase::Error;
        self.orchestration.current_tool = None;
        self.orchestration.active_task = format!("Error: {}", short_text(msg, 56));
        self.push_event(
            "main",
            format!("error: {}", short_text(msg, 36)),
            Palette::RED,
        );
    }

    pub fn subagent_tiles(&self) -> Vec<SubAgentTile> {
        let mut log = Vec::new();
        log.push(self.orchestration.active_task.clone());
        if let Some(tool) = &self.orchestration.current_tool {
            log.push(format!("current tool · {tool}"));
        }
        for event in self.orchestration.recent_events.iter().rev().take(2).rev() {
            log.push(event.msg.clone());
        }
        if let Some(reasoning) = self.latest_reasoning() {
            log.push(format!("reasoning · {}", short_text(reasoning, 48)));
        }
        vec![SubAgentTile {
            name: "main".into(),
            role: "orchestrator".into(),
            state: self.orchestration.phase.label().into(),
            color: self.orchestration.phase.color(),
            log,
            cpu: Vec::new(),
        }]
    }

    pub fn ticker_cells(&self) -> Vec<TickerCell> {
        let recent: Vec<TickerCell> = self
            .orchestration
            .recent_events
            .iter()
            .rev()
            .take(4)
            .map(|e| TickerCell {
                sub: e.actor.clone(),
                msg: e.msg.clone(),
                color: e.color,
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if recent.is_empty() {
            vec![TickerCell {
                sub: "main".into(),
                msg: self.orchestration.active_task.clone(),
                color: self.orchestration.phase.color(),
            }]
        } else {
            recent
        }
    }

    pub fn tree_nodes(&self) -> Vec<TreeNode> {
        let mut nodes = vec![TreeNode {
            depth: 0,
            label: format!("root · {}", self.orchestration.active_task),
            state: self.orchestration.phase.symbol().into(),
            color: self.orchestration.phase.color(),
            active: true,
        }];
        if let Some(tool) = &self.orchestration.current_tool {
            nodes.push(TreeNode {
                depth: 1,
                label: format!("└─ tool · {tool}"),
                state: OrchestrationPhase::ToolRunning.symbol().into(),
                color: Palette::BLUE,
                active: true,
            });
        }
        nodes
    }

    pub fn delegated_worker_count(&self) -> usize {
        0
    }

    pub fn branch_count(&self) -> usize {
        self.tree_nodes().len().saturating_sub(1)
    }

    pub fn latest_reasoning(&self) -> Option<&str> {
        self.messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, ChatRole::Agent) && !m.reasoning.is_empty())
            .map(|m| m.reasoning.as_str())
    }

    pub fn latest_agent_content(&self) -> Option<&str> {
        self.messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, ChatRole::Agent) && !m.content.is_empty())
            .map(|m| m.content.as_str())
    }

    pub fn active_session(&self) -> Option<&SessionState> {
        self.sessions.iter().find(|session| session.is_active)
    }

    fn push_event(&mut self, actor: &str, msg: String, color: Color) {
        self.orchestration.recent_events.push(OrchestrationEvent {
            actor: actor.into(),
            msg,
            color,
        });
        if self.orchestration.recent_events.len() > 12 {
            let overflow = self.orchestration.recent_events.len() - 12;
            self.orchestration.recent_events.drain(0..overflow);
        }
    }
}

/// Compress a long tool name for the actor column (5 chars wide).
fn short_tool(name: &str) -> String {
    if name.len() <= 5 {
        return name.to_string();
    }
    name.chars().take(5).collect()
}

fn short_session_id(id: &str) -> String {
    let short: String = id.chars().take(8).collect();
    format!("session-{short}")
}

fn demo_tool_log() -> Vec<ToolLogRow> {
    vec![
        ToolLogRow {
            time: "14:02:13".into(),
            actor: "main".into(),
            msg: "spawn ×3".into(),
        },
        ToolLogRow {
            time: "14:02:15".into(),
            actor: "a3f".into(),
            msg: "read_file ✓".into(),
        },
        ToolLogRow {
            time: "14:02:18".into(),
            actor: "b21".into(),
            msg: "grep ✓ 14m".into(),
        },
        ToolLogRow {
            time: "14:02:22".into(),
            actor: "a3f".into(),
            msg: "edit ✓ 12h".into(),
        },
        ToolLogRow {
            time: "14:02:25".into(),
            actor: "b21".into(),
            msg: "edit ●".into(),
        },
        ToolLogRow {
            time: "14:02:29".into(),
            actor: "a3f".into(),
            msg: "cargo ✓".into(),
        },
        ToolLogRow {
            time: "14:02:31".into(),
            actor: "main".into(),
            msg: "merge ?".into(),
        },
    ]
}

fn demo_diff() -> Vec<DiffLine> {
    vec![
        DiffLine {
            text: "@@ -88,6 +88,8 @@".into(),
            kind: DiffKind::Hunk,
        },
        DiffLine {
            text: "- pub fn verify(&self, t: &Token) {".into(),
            kind: DiffKind::Removed,
        },
        DiffLine {
            text: "+ pub fn verify<'a>(&self, t: &'a Token) -> Result<Claims> {".into(),
            kind: DiffKind::Added,
        },
        DiffLine {
            text: "    let claims = decode(t)?;".into(),
            kind: DiffKind::Ctx,
        },
        DiffLine {
            text: "+   self.audit.log(&claims);".into(),
            kind: DiffKind::Added,
        },
        DiffLine {
            text: "    Ok(claims)".into(),
            kind: DiffKind::Ctx,
        },
    ]
}

fn short_text(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::SessionSummary;

    #[test]
    fn orchestration_state_tracks_prompt_and_tool_flow() {
        let mut app = AppState::new("test-model".into(), 128_000);
        app.note_prompt_submitted("list files in the current directory");
        assert_eq!(app.orchestration.phase, OrchestrationPhase::Thinking);
        assert!(app.orchestration.active_task.contains("list files"));

        app.note_tool_start("bash");
        assert_eq!(app.orchestration.phase, OrchestrationPhase::ToolRunning);
        assert_eq!(app.orchestration.current_tool.as_deref(), Some("bash"));

        let tree = app.tree_nodes();
        assert_eq!(tree.len(), 2);
        assert!(tree[1].label.contains("bash"));

        app.note_tool_end("bash", true);
        assert_eq!(app.orchestration.phase, OrchestrationPhase::Thinking);
        assert!(app.orchestration.current_tool.is_none());

        app.note_done();
        assert_eq!(app.orchestration.phase, OrchestrationPhase::Complete);
        assert_eq!(app.ticker_cells().last().unwrap().msg, "completed turn");
    }

    #[test]
    fn subagent_tiles_expose_single_real_orchestrator() {
        let mut app = AppState::new("test-model".into(), 128_000);
        app.note_prompt_submitted("check auth middleware");
        let tiles = app.subagent_tiles();
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].name, "main");
        assert_eq!(tiles[0].role, "orchestrator");
        assert!(tiles[0].log[0].contains("check auth middleware"));
    }

    #[test]
    fn hydrate_sessions_retains_lineage_and_activity_fields() {
        let mut app = AppState::new("test-model".into(), 128_000);
        let parent_id = "parent-12345678".to_string();
        let child_id = "child-87654321".to_string();
        app.hydrate_sessions(
            &[SessionSummary {
                id: child_id.clone(),
                created_at: 10,
                last_active: 20,
                message_count: 3,
                parent_session_id: Some(parent_id.clone()),
                lineage_label: Some("branched from auth cleanup".into()),
            }],
            &child_id,
        );

        assert_eq!(app.sessions.len(), 1);
        let session = &app.sessions[0];
        assert_eq!(session.id, child_id);
        assert_eq!(
            session.parent_session_id.as_deref(),
            Some(parent_id.as_str())
        );
        assert_eq!(
            session.lineage_label.as_deref(),
            Some("branched from auth cleanup")
        );
        assert_eq!(session.status, SessionStatus::Live);
        assert!(session.is_active);
        assert_eq!(session.message_count, 3);
    }

    #[test]
    fn segments_interleave_reasoning_tool_text_in_arrival_order() {
        // Simulates the exact YYC-71 sequence: think → tool → think → answer.
        let mut m = ChatMessage::new(ChatRole::Agent, "");
        m.append_reasoning("checking the file");
        m.push_tool_start("read_file");
        m.finish_tool("read_file", true);
        m.append_reasoning("now writing");
        m.push_tool_start("write_file");
        m.finish_tool("write_file", true);
        m.append_text("Done!");

        let kinds: Vec<&str> = m
            .segments
            .iter()
            .map(|s| match s {
                MessageSegment::Reasoning(_) => "reasoning",
                MessageSegment::Text(_) => "text",
                MessageSegment::ToolCall { .. } => "tool",
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["reasoning", "tool", "reasoning", "tool", "text"]
        );
    }

    #[test]
    fn append_reasoning_coalesces_until_broken_by_other_segment() {
        let mut m = ChatMessage::new(ChatRole::Agent, "");
        m.append_reasoning("first ");
        m.append_reasoning("chunk");
        m.push_tool_start("bash");
        m.append_reasoning("after tool");
        // Three segments: reasoning, tool, reasoning — second reasoning is
        // its own segment because the tool call broke the run.
        assert_eq!(m.segments.len(), 3);
        match &m.segments[0] {
            MessageSegment::Reasoning(r) => assert_eq!(r, "first chunk"),
            other => panic!("expected reasoning, got {other:?}"),
        }
        match &m.segments[2] {
            MessageSegment::Reasoning(r) => assert_eq!(r, "after tool"),
            other => panic!("expected reasoning, got {other:?}"),
        }
    }

    #[test]
    fn refresh_prompt_mode_picks_command_when_input_starts_with_slash() {
        let mut app = AppState::new("test".into(), 100);
        app.input = "/help".into();
        app.refresh_prompt_mode();
        assert_eq!(app.prompt_mode, PromptMode::Command);
    }

    #[test]
    fn refresh_prompt_mode_returns_to_insert_when_slash_cleared() {
        let mut app = AppState::new("test".into(), 100);
        app.input = "/help".into();
        app.refresh_prompt_mode();
        app.input.clear();
        app.refresh_prompt_mode();
        assert_eq!(app.prompt_mode, PromptMode::Insert);
    }

    #[test]
    fn refresh_prompt_mode_uses_busy_when_thinking() {
        let mut app = AppState::new("test".into(), 100);
        app.thinking = true;
        app.refresh_prompt_mode();
        assert_eq!(app.prompt_mode, PromptMode::Busy);
        assert_eq!(app.mode_label(), "BUSY");
    }

    #[test]
    fn refresh_prompt_mode_busy_overrides_command_prefix() {
        // While the agent is mid-turn the badge should still read BUSY
        // even if the user typed `/` in the prompt — the slash menu can
        // be shown but the mode pill reflects the agent state.
        let mut app = AppState::new("test".into(), 100);
        app.thinking = true;
        app.input = "/queue".into();
        app.refresh_prompt_mode();
        assert_eq!(app.prompt_mode, PromptMode::Busy);
    }

    #[test]
    fn finish_tool_pairs_with_most_recent_in_progress_call_of_same_name() {
        // Parallel dispatch (YYC-34): two write_file calls in flight; the
        // first to finish should pair with the most recent matching start
        // that's still in-progress.
        let mut m = ChatMessage::new(ChatRole::Agent, "");
        m.push_tool_start("write_file");
        m.push_tool_start("write_file");
        m.finish_tool("write_file", true);

        let statuses: Vec<ToolStatus> = m
            .segments
            .iter()
            .filter_map(|s| match s {
                MessageSegment::ToolCall { status, .. } => Some(*status),
                _ => None,
            })
            .collect();
        // Most-recent in-progress finishes first; the older one stays open.
        assert_eq!(
            statuses,
            vec![ToolStatus::InProgress, ToolStatus::Done(true)]
        );
    }
}
