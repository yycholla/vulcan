use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use super::keybinds::Keybinds;

/// Format a u32 with comma thousands separators (YYC-60).
/// `18402 → "18,402"`. Pure utility, no allocation beyond the result.
pub fn format_thousands(n: u32) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

use chrono::Local;
use ratatui::style::Color;

use crate::hooks::audit::{AuditBuffer, AuditKind};
use crate::memory::SessionSummary;

use super::theme::{Palette, Theme};
use super::views::{DiffKind, DiffLine, View};

/// Diff render style (YYC-77). Toggled by `/diff-style`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiffStyle {
    /// Classic `+ -` lines, single column. Default.
    #[default]
    Unified,
    /// Before / after columns separated by `│`.
    SideBySide,
    /// Word-level highlight on the new line; useful for tiny edits.
    Inline,
}

impl DiffStyle {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unified => "unified",
            Self::SideBySide => "side-by-side",
            Self::Inline => "inline",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::Unified => Self::SideBySide,
            Self::SideBySide => Self::Inline,
            Self::Inline => Self::Unified,
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "unified" | "u" => Some(Self::Unified),
            "side-by-side" | "split" | "sbs" | "s" => Some(Self::SideBySide),
            "inline" | "i" => Some(Self::Inline),
            _ => None,
        }
    }
}

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

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
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
    /// One tool invocation rendered as a structured card (YYC-74).
    /// `params_summary` is the one-line projection from the agent's
    /// `summarize_tool_args` (e.g. path for file ops, query for
    /// search). `output_preview` is a truncated tail of the tool
    /// result. `elapsed_ms` is wall-clock dispatch time for the
    /// timing note. All optional — older streams that don't populate
    /// them still render a minimal card.
    ToolCall {
        name: String,
        status: ToolStatus,
        params_summary: Option<String>,
        output_preview: Option<String>,
        /// One-line metadata derived from tool result (e.g. "847 lines",
        /// "5 matches", "+12 -3"). Renders as a dimmed sub-header in
        /// the YYC-74 card.
        result_meta: Option<String>,
        /// Lines elided beyond the preview (YYC-78). When > 0 the card
        /// renders a "… N more lines elided" footer.
        elided_lines: usize,
        elapsed_ms: Option<u64>,
    },
}

impl MessageSegment {
    /// Stable kind tag used by the renderer to detect transitions between
    /// segment types and insert a blank-line separator (YYC-91).
    pub fn kind_label(&self) -> &'static str {
        match self {
            MessageSegment::Reasoning(_) => "reasoning",
            MessageSegment::Text(_) => "text",
            MessageSegment::ToolCall { .. } => "tool",
        }
    }
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
    pub(crate) render_version: u64,
}

impl ChatMessage {
    pub fn new(role: ChatRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            reasoning: String::new(),
            segments: Vec::new(),
            render_version: 0,
        }
    }

    pub fn render_version(&self) -> u64 {
        self.render_version
    }

    fn bump_render_version(&mut self) {
        self.render_version = self.render_version.wrapping_add(1);
    }

    pub fn set_content(&mut self, content: impl Into<String>) {
        self.content = content.into();
        self.bump_render_version();
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
        self.bump_render_version();
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
        self.bump_render_version();
    }

    pub fn push_tool_start(&mut self, name: impl Into<String>) {
        self.push_tool_start_with(name, None);
    }

    /// Push a tool-call segment with the params summary the YYC-74
    /// card needs.
    pub fn push_tool_start_with(
        &mut self,
        name: impl Into<String>,
        params_summary: Option<String>,
    ) {
        self.segments.push(MessageSegment::ToolCall {
            name: name.into(),
            status: ToolStatus::InProgress,
            params_summary,
            output_preview: None,
            result_meta: None,
            elided_lines: 0,
            elapsed_ms: None,
        });
        self.bump_render_version();
    }

    /// Mark the most recent in-progress ToolCall with this name as done.
    /// Walks segments in reverse so concurrent dispatch (YYC-34) still
    /// pairs each end with its own start as the matching tail.
    pub fn finish_tool(&mut self, name: &str, ok: bool) {
        self.finish_tool_with(name, ok, None, None, 0, None);
    }

    /// Same as `finish_tool` but also stamps the result preview, meta
    /// summary, elided count, and timing for the YYC-74 card.
    pub fn finish_tool_with(
        &mut self,
        name: &str,
        ok: bool,
        output_preview: Option<String>,
        result_meta: Option<String>,
        elided_lines: usize,
        elapsed_ms: Option<u64>,
    ) {
        for seg in self.segments.iter_mut().rev() {
            if let MessageSegment::ToolCall {
                name: n,
                status,
                output_preview: op,
                result_meta: rm,
                elided_lines: el,
                elapsed_ms: em,
                ..
            } = seg
            {
                if n == name && matches!(status, ToolStatus::InProgress) {
                    *status = ToolStatus::Done(ok);
                    *op = output_preview;
                    *rm = result_meta;
                    *el = elided_lines;
                    *em = elapsed_ms;
                    self.bump_render_version();
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
    pub preview: Option<String>,
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

/// One row in the provider picker overlay (YYC-97). `None` name = the
/// legacy unnamed `[provider]` block; `Some(name)` = a `[providers.<name>]`
/// profile.
#[derive(Clone, Debug)]
pub struct ProviderPickerEntry {
    pub name: Option<String>,
    pub model: String,
    pub base_url: String,
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
    /// Diff render style (YYC-77). Toggled via `/diff-style`.
    pub diff_style: DiffStyle,
    /// Pending prompts submitted while the agent was busy (YYC-61).
    /// Drained one-at-a-time from the front when each turn completes.
    /// In-memory only — never persisted to sessions.db.
    pub queue: VecDeque<String>,
    pub show_reasoning: bool,
    pub session_label: String,

    pub sessions: Vec<SessionState>,
    pub active_session_id: Option<String>,
    pub diff_lines: Vec<DiffLine>,
    /// Live edit-diff sink shared with the agent (YYC-66). Renderers
    /// peek the inner Option each frame to surface the latest edit; we
    /// fall back to `diff_lines` (demo) only until the first real edit
    /// arrives, after which the live diff replaces it.
    pub diff_sink: Option<crate::tools::EditDiffSink>,
    /// Per-token pricing pulled from the provider catalog at startup
    /// (YYC-67). Used with `prompt_tokens_total + completion_tokens_total`
    /// to compute estimated session cost.
    pub pricing: Option<crate::provider::catalog::Pricing>,
    /// Number of tool dispatches this session (counts every ToolCallEnd).
    pub tool_calls_total: u32,
    /// Tool dispatches that ended with `ok=false`.
    pub tool_errors_total: u32,
    /// Provider-level errors surfaced via StreamEvent::Error.
    pub provider_errors_total: u32,
    /// When the TUI session started — used for elapsed-time displays.
    pub session_started: std::time::Instant,
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
    /// Active named provider profile, if any (YYC-96). `None` means the
    /// session is running on the legacy unnamed `[provider]` block —
    /// `model_status()` omits the prefix in that case.
    pub provider_label: Option<String>,
    /// Cumulative input tokens across the whole session (every turn's
    /// `usage.prompt_tokens` summed). Used by the cost telemetry pane
    /// (YYC-67).
    pub prompt_tokens_total: u32,
    /// Cumulative output tokens across the whole session.
    pub completion_tokens_total: u32,
    /// Latest turn's `usage.prompt_tokens` — represents the current
    /// context window size and is what the prompt-status ratio bar
    /// (YYC-60) uses for capacity coloring.
    pub prompt_tokens_last: u32,
    pub token_max: u32,
    pub chat_render_store: RefCell<super::chat_render::ChatRenderStore>,

    /// When true, overlays a session picker on top of the normal view.
    /// Set by `ResumeTarget::Pick` at startup; cleared when the user
    /// selects a session or dismisses with Esc.
    pub show_session_picker: bool,
    /// Index into `sessions` for the highlighted row in the picker.
    pub session_picker_selection: usize,

    /// Model picker overlay (YYC-97 → YYC-101 hierarchical). Opened by
    /// `/model` with no args; items are populated from the active
    /// provider's catalog at open time. Tree columns drill
    /// lab → series → version. `Enter` triggers `Agent::switch_model`.
    pub show_model_picker: bool,
    /// Source catalog for the picker; rebuilt each open.
    pub model_picker_items: Vec<crate::provider::catalog::ModelInfo>,
    /// Hierarchical tree built from `model_picker_items`.
    pub model_picker_tree: super::model_picker::ModelTree,
    /// Selection index per drilled column.
    pub model_picker_path: Vec<usize>,
    /// Which column currently has focus (0 = column 0, etc.).
    pub model_picker_focus: usize,

    /// Provider picker overlay (YYC-97). Opened by `/provider` with no
    /// args; items are the legacy `[provider]` block followed by named
    /// `[providers.<name>]` profiles. `name = None` is the legacy entry.
    pub show_provider_picker: bool,
    pub provider_picker_selection: usize,
    pub provider_picker_items: Vec<ProviderPickerEntry>,

    /// Diff scrubber overlay (YYC-75). Opened when `edit_file` matches
    /// multiple sites and the pause channel is wired. Each hunk
    /// individually opt-in/out via `scrubber_accepted`.
    pub show_diff_scrubber: bool,
    pub scrubber_path: String,
    pub scrubber_hunks: Vec<crate::pause::DiffScrubHunk>,
    pub scrubber_accepted: Vec<bool>,
    pub scrubber_selection: usize,
    /// The pause we'll reply to when the user resolves the scrubber.
    /// Stored separately from `pending_pause` so it doesn't conflict
    /// with the pill-style prompts.
    pub scrubber_pause: Option<crate::pause::AgentPause>,

    /// Active TUI theme — render code reads role styles via `state.theme.<role>`.
    /// Defaults to `Theme::system()` in `AppState::new` so unconfigured/test
    /// callers inherit terminal palette; production wires the real config-derived
    /// theme via `.with_theme(...)` post-construction.
    pub theme: Theme,

    /// Active key bindings — drives both the input handler in `tui::mod`
    /// and the prompt-row hint cache below (YYC-90).
    pub keybinds: Keybinds,
    /// Pre-formatted hint pairs per `PromptMode`. Built once in
    /// `with_keybinds`; `prompt_hints()` returns a slice into one of
    /// these vectors so the render hot path never allocates.
    prompt_hints_cache: PromptHintsCache,
}

#[derive(Clone, Debug)]
struct PromptHintsCache {
    insert: Vec<(String, String)>,
    command: Vec<(String, String)>,
    ask: Vec<(String, String)>,
    busy: Vec<(String, String)>,
}

impl PromptHintsCache {
    fn build(kb: &Keybinds) -> Self {
        Self {
            insert: vec![
                ("Enter".into(), "send".into()),
                (kb.toggle_tools.label(), "tools".into()),
                (kb.toggle_sessions.label(), "sessions".into()),
                ("/".into(), "cmds".into()),
            ],
            command: vec![
                ("Enter".into(), "run".into()),
                ("Up/Dn".into(), "select".into()),
                ("Tab".into(), "complete".into()),
                ("Esc".into(), "cancel".into()),
            ],
            ask: vec![
                ("y".into(), "proceed".into()),
                ("n".into(), "deny".into()),
                ("Esc".into(), "cancel".into()),
            ],
            busy: vec![
                ("Enter".into(), "queue".into()),
                (kb.cancel.label(), "cancel".into()),
                (kb.queue_drop.label(), "drop last".into()),
            ],
        }
    }
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
            diff_style: DiffStyle::default(),
            queue: VecDeque::new(),
            show_reasoning: true,
            session_label: "new session".into(),

            sessions: Vec::new(),
            active_session_id: None,
            diff_lines: demo_diff(),
            diff_sink: None,
            pricing: None,
            tool_calls_total: 0,
            tool_errors_total: 0,
            provider_errors_total: 0,
            session_started: std::time::Instant::now(),
            orchestration: OrchestrationState::default(),

            audit_log: None,
            pending_pause: None,

            cursor: Cell::new((0, 0)),
            model_label,
            provider_label: None,
            prompt_tokens_total: 0,
            completion_tokens_total: 0,
            prompt_tokens_last: 0,
            token_max,
            chat_render_store: RefCell::new(super::chat_render::ChatRenderStore::default()),

            show_session_picker: false,
            session_picker_selection: 0,

            show_model_picker: false,
            model_picker_items: Vec::new(),
            model_picker_tree: super::model_picker::ModelTree::default(),
            model_picker_path: Vec::new(),
            model_picker_focus: 0,

            show_provider_picker: false,
            provider_picker_selection: 0,
            provider_picker_items: Vec::new(),

            show_diff_scrubber: false,
            scrubber_path: String::new(),
            scrubber_hunks: Vec::new(),
            scrubber_accepted: Vec::new(),
            scrubber_selection: 0,
            scrubber_pause: None,

            theme: Theme::system(),
            keybinds: Keybinds::default(),
            prompt_hints_cache: PromptHintsCache::build(&Keybinds::default()),
        }
    }

    /// Replace the active theme. Used by the TUI entrypoint to swap the
    /// default `Theme::system()` for the user's configured theme.
    pub fn with_theme(mut self, theme: Theme) -> Self {
        self.theme = theme;
        self
    }

    /// Replace the active key bindings and rebuild the prompt-row hint
    /// cache (YYC-90). Builder-style so existing `AppState::new` call
    /// sites don't change.
    pub fn with_keybinds(mut self, keybinds: Keybinds) -> Self {
        self.prompt_hints_cache = PromptHintsCache::build(&keybinds);
        self.keybinds = keybinds;
        self
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

    /// Per-mode hint pairs for the prompt-row footer (YYC-58, YYC-90).
    /// Returns a slice into the cached, pre-formatted vector so the
    /// render hot path never allocates. Bindings come from `self.keybinds`.
    pub fn prompt_hints(&self) -> &[(String, String)] {
        match self.prompt_mode {
            PromptMode::Insert => &self.prompt_hints_cache.insert,
            PromptMode::Command => &self.prompt_hints_cache.command,
            PromptMode::Ask => &self.prompt_hints_cache.ask,
            PromptMode::Busy => &self.prompt_hints_cache.busy,
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
        // YYC-60: design spec is `{model} · {used:n} / {max:n}` with comma
        // grouping. `used` is the latest turn's prompt_tokens (current
        // context window size); the bar represents context capacity, not
        // lifetime cost.
        // YYC-96: when running on a named provider profile, prefix the
        // string with `{profile} · ` so the user can tell which provider
        // any given turn will hit.
        match &self.provider_label {
            Some(profile) => format!(
                "{} · {} · {} / {}",
                profile,
                self.model_label,
                format_thousands(self.prompt_tokens_last),
                format_thousands(self.token_max),
            ),
            None => format!(
                "{} · {} / {}",
                self.model_label,
                format_thousands(self.prompt_tokens_last),
                format_thousands(self.token_max),
            ),
        }
    }

    /// Estimated session cost in USD, computed from cumulative token
    /// totals (YYC-60) and per-token pricing (YYC-67). `None` when
    /// pricing isn't available — the renderer should show "—" rather
    /// than a fake number.
    pub fn estimated_cost(&self) -> Option<f64> {
        let p = self.pricing.as_ref()?;
        Some(
            (self.prompt_tokens_total as f64) * p.input_per_token
                + (self.completion_tokens_total as f64) * p.output_per_token,
        )
    }

    /// Snapshot the most recent file edit, if the diff sink is wired up
    /// and an edit has been captured (YYC-66). Locks briefly to clone the
    /// value so the renderer doesn't hold the mutex across draws.
    pub fn latest_diff(&self) -> Option<crate::tools::EditDiff> {
        let sink = self.diff_sink.as_ref()?;
        sink.lock().ok()?.clone()
    }

    /// Cumulative token count (input + output across all turns). Used by
    /// the cost/runtime telemetry pane (YYC-67).
    pub fn lifetime_tokens(&self) -> u32 {
        self.prompt_tokens_total
            .saturating_add(self.completion_tokens_total)
    }

    /// Ratio of latest context to max — drives the capacity coloring on
    /// the prompt-status row (YYC-60).
    pub fn context_ratio(&self) -> f32 {
        if self.token_max == 0 {
            return 0.0;
        }
        (self.prompt_tokens_last as f32) / (self.token_max as f32)
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
                    preview: s.preview.clone(),
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
                provider_profile: None,
                preview: None,
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
    fn chat_message_render_version_bumps_on_mutation() {
        let mut m = ChatMessage::new(ChatRole::Agent, "");
        let initial = m.render_version();

        m.append_text("hello");
        assert!(m.render_version() > initial);
        let after_text = m.render_version();

        m.append_reasoning("thinking");
        assert!(m.render_version() > after_text);
        let after_reasoning = m.render_version();

        m.push_tool_start("bash");
        assert!(m.render_version() > after_reasoning);
        let after_tool_start = m.render_version();

        m.finish_tool("bash", true);
        assert!(m.render_version() > after_tool_start);
    }

    #[test]
    fn model_status_omits_prefix_when_no_provider_label() {
        let mut app = AppState::new("deepseek/v4".into(), 128_000);
        app.prompt_tokens_last = 18_402;
        let status = app.model_status();
        assert_eq!(status, "deepseek/v4 · 18,402 / 128,000");
        assert!(!status.contains(" · deepseek/v4"));
    }

    #[test]
    fn model_status_prefixes_active_provider_label() {
        let mut app = AppState::new("deepseek/v4".into(), 128_000);
        app.provider_label = Some("local".into());
        app.prompt_tokens_last = 18_402;
        let status = app.model_status();
        assert_eq!(status, "local · deepseek/v4 · 18,402 / 128,000");
    }

    #[test]
    fn chat_message_new_starts_at_zero_render_version() {
        let m = ChatMessage::new(ChatRole::User, "hello");
        assert_eq!(m.render_version(), 0);
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
    fn prompt_hints_default_keybinds_match_ascii_labels() {
        // Default Keybinds should produce ASCII-safe labels (Ctrl+T,
        // Ctrl+K) so prompt-row chips render in any terminal font.
        let app = AppState::new("test".into(), 100);
        let hints = app.prompt_hints();
        let pairs: Vec<(String, String)> = hints.iter().cloned().collect();
        assert!(
            pairs.contains(&("Ctrl+T".into(), "tools".into())),
            "expected Ctrl+T tools in {pairs:?}"
        );
        assert!(
            pairs.contains(&("Ctrl+K".into(), "sessions".into())),
            "expected Ctrl+K sessions in {pairs:?}"
        );
    }

    #[test]
    fn prompt_hints_reflect_overridden_keybind() {
        use super::super::keybinds::{KeyBinding, Keybinds};
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut kb = Keybinds::defaults();
        kb.toggle_tools = KeyBinding {
            code: KeyCode::F(2),
            mods: KeyModifiers::NONE,
        };
        let app = AppState::new("test".into(), 100).with_keybinds(kb);
        let pairs: Vec<(String, String)> = app.prompt_hints().iter().cloned().collect();
        assert!(
            pairs.contains(&("F2".into(), "tools".into())),
            "expected F2 tools in {pairs:?}"
        );
        assert!(
            !pairs.iter().any(|(k, _)| k == "Ctrl+T"),
            "stale Ctrl+T label leaked into {pairs:?}"
        );
    }

    #[test]
    fn prompt_hints_returns_borrowed_slice_no_alloc() {
        // Two consecutive calls in the same mode must return slices to
        // the same cached storage — proves we're not allocating per call.
        let app = AppState::new("test".into(), 100);
        let first = app.prompt_hints().as_ptr();
        let second = app.prompt_hints().as_ptr();
        assert_eq!(first, second, "prompt_hints reallocated between calls");
    }

    #[test]
    fn format_thousands_groups_at_three_digit_boundaries() {
        assert_eq!(format_thousands(0), "0");
        assert_eq!(format_thousands(42), "42");
        assert_eq!(format_thousands(999), "999");
        assert_eq!(format_thousands(1_000), "1,000");
        assert_eq!(format_thousands(18_402), "18,402");
        assert_eq!(format_thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn context_ratio_reflects_latest_prompt_size() {
        let mut app = AppState::new("test".into(), 100_000);
        assert_eq!(app.context_ratio(), 0.0);
        app.prompt_tokens_last = 50_000;
        assert!((app.context_ratio() - 0.5).abs() < 1e-6);
        app.prompt_tokens_last = 95_000;
        assert!(app.context_ratio() > 0.9);
    }

    #[test]
    fn estimated_cost_returns_none_without_pricing() {
        let mut app = AppState::new("test".into(), 100_000);
        app.prompt_tokens_total = 1_000;
        app.completion_tokens_total = 500;
        assert!(app.estimated_cost().is_none());
    }

    #[test]
    fn estimated_cost_multiplies_tokens_by_per_token_pricing() {
        let mut app = AppState::new("test".into(), 100_000);
        app.prompt_tokens_total = 1_000;
        app.completion_tokens_total = 500;
        app.pricing = Some(crate::provider::catalog::Pricing {
            input_per_token: 0.000_001,
            output_per_token: 0.000_002,
        });
        let cost = app.estimated_cost().unwrap();
        // 1000*0.000001 + 500*0.000002 = 0.001 + 0.001 = 0.002
        assert!((cost - 0.002).abs() < 1e-9, "got {cost}");
    }

    #[test]
    fn lifetime_tokens_sums_prompt_and_completion_totals() {
        let mut app = AppState::new("test".into(), 100_000);
        app.prompt_tokens_total = 1_000;
        app.completion_tokens_total = 500;
        assert_eq!(app.lifetime_tokens(), 1_500);
    }

    #[test]
    fn queue_starts_empty_and_pushes_in_fifo_order() {
        let mut app = AppState::new("test".into(), 100);
        assert!(app.queue.is_empty());
        app.queue.push_back("first".into());
        app.queue.push_back("second".into());
        assert_eq!(app.queue.pop_front().as_deref(), Some("first"));
        assert_eq!(app.queue.pop_front().as_deref(), Some("second"));
        assert!(app.queue.is_empty());
    }

    #[test]
    fn queue_pop_back_removes_most_recent_only() {
        let mut app = AppState::new("test".into(), 100);
        app.queue.push_back("a".into());
        app.queue.push_back("b".into());
        app.queue.push_back("c".into());
        app.queue.pop_back();
        let remaining: Vec<&str> = app.queue.iter().map(String::as_str).collect();
        assert_eq!(remaining, vec!["a", "b"]);
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

    #[test]
    fn push_tool_start_with_carries_params_summary() {
        let mut m = ChatMessage::new(ChatRole::Agent, "");
        m.push_tool_start_with("read_file", Some("src/foo.rs".into()));
        match &m.segments[0] {
            MessageSegment::ToolCall { params_summary, .. } => {
                assert_eq!(params_summary.as_deref(), Some("src/foo.rs"))
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn finish_tool_with_stamps_preview_and_timing() {
        let mut m = ChatMessage::new(ChatRole::Agent, "");
        m.push_tool_start("read_file");
        m.finish_tool_with(
            "read_file",
            true,
            Some("hello\nworld".into()),
            Some("2 lines".into()),
            0,
            Some(345),
        );
        match &m.segments[0] {
            MessageSegment::ToolCall {
                output_preview,
                elapsed_ms,
                status,
                ..
            } => {
                assert!(matches!(status, ToolStatus::Done(true)));
                assert_eq!(output_preview.as_deref(), Some("hello\nworld"));
                assert_eq!(*elapsed_ms, Some(345));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }
}
