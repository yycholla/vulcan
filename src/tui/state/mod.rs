//! TUI application state.
//!
//! # Threading model (YYC-163)
//!
//! `AppState` is **single-threaded**. The TUI's main loop owns it
//! and is the only place that reads or writes it; rendering, input
//! handling, and persistence all happen on that same thread. The
//! type therefore uses `std::cell::Cell` and `std::cell::RefCell`
//! for interior mutability instead of `Mutex` / `RwLock` — there is
//! no synchronization to pay because there is no contention.
//!
//! ## Why this matters
//!
//! `Cell`/`RefCell` are `!Sync`. A future refactor that tries to
//! move rendering or event handling onto another thread will hit a
//! compile error rather than a silent borrow-tracking panic, which
//! is what we want — the right answer in that scenario is to
//! either keep the work on the TUI thread (preferred) or replace
//! the affected fields with proper synchronization primitives.
//!
//! ## What stays thread-confined
//!
//! The whole `AppState` value, plus everything it transitively
//! reaches through `Cell` / `RefCell`. Anything that needs to be
//! shared with worker tasks (the agent loop, the audit buffer, the
//! diff sink) is wrapped in `Arc<Mutex<…>>` or
//! `Arc<parking_lot::Mutex<…>>` separately — see the field-level
//! documentation below for which ones cross threads on purpose.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use super::input::TuiKeyEvent;
use super::keybinds::Keybinds;

/// Format a u32 with comma thousands separators (YYC-60).
/// `18402 → "18,402"`. Pure utility, no allocation beyond the result.
pub fn format_thousands(n: u32) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

use chrono::Local;
use ratatui::style::Color;
use throbber_widgets_tui::ThrobberState;

use crate::hooks::audit::{AuditBuffer, AuditKind};
use crate::memory::SessionSummary;

// Re-export the types pulled into sibling submodules in YYC-110 so the
// legacy `tui::state::*` import paths used across the TUI still resolve.
pub use super::chat_message::{ChatMessage, MessageSegment};
pub use super::orchestration::{
    OrchestrationEvent, OrchestrationPhase, OrchestrationState, SubAgentTile, TickerCell,
    ToolLogRow, TreeNode,
};
pub use super::picker_state::{ProviderPickerEntry, SessionState, SessionStatus};
pub use super::prompt::{PromptEditMode, PromptEditor, PromptEnterIntent, PromptEscapeIntent};

use super::diff_scrubber::{DiffScrubberOutcome, DiffScrubberState};
use super::effects::TuiEffects;
use super::model_picker::{ModelPickerOutcome, ModelPickerState};
use super::pause_prompt::{PausePromptOutcome, PausePromptState};
use super::provider_picker::ProviderPickerOutcome;
use super::surface::SurfaceFrame;
use super::theme::{Palette, Theme};
use super::ui_runtime::UiRuntime;
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

pub struct ActiveTick {
    pub rate: vulcan_frontend_api::TickRate,
    pub handle: vulcan_frontend_api::TickHandle,
    callback: std::sync::Arc<dyn Fn(&vulcan_frontend_api::TickHandle) + Send + Sync>,
    next_fire: Instant,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ChatClearPhase {
    #[default]
    Idle,
    Requested,
    Exploding,
    RevealRequested,
    Revealing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelScope {
    Turn,
    Dialog,
    Canvas,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelPop {
    Popped(CancelScope),
    CancelTurn,
    None,
}

pub struct AppState {
    pub view: View,
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub prompt_editor: PromptEditor,
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
    /// Per YYC-125, plain-text submissions during an agent turn act as
    /// "steers": they queue here and the entire batch fires as one
    /// combined user message at the next turn-end Done event.
    /// In-memory only — never persisted to sessions.db.
    pub queue: VecDeque<String>,
    /// Explicit `/queue <msg>` deferrals (YYC-125). Strict FIFO post-
    /// turn drain — one message per Done event, after the steer batch
    /// has flushed.
    pub deferred_queue: VecDeque<String>,
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
    /// YYC-207: live snapshot store for child agent runs. When
    /// populated, `subagent_tiles` / `tree_nodes` consult it for
    /// real records; when `None`, those methods fall back to the
    /// legacy single-"main" tile shape so demo / test paths still
    /// render coherently.
    pub orchestration_store: Option<std::sync::Arc<crate::orchestration::OrchestrationStore>>,

    /// Optional shared handle to the audit-log hook's ring buffer. When
    /// present, the trading-floor tool-log pane renders from it; otherwise it
    /// falls back to demo data so the design still looks alive on first launch.
    pub audit_log: Option<AuditBuffer>,
    pub frontend: super::frontend::TuiFrontend,
    status_widgets: BTreeMap<String, vulcan_frontend_api::WidgetContent>,
    ui_runtime: UiRuntime,
    pub active_ticks: Vec<ActiveTick>,
    pub activity_throbber: ThrobberState,
    pub effects: TuiEffects,
    chat_clear_phase: Cell<ChatClearPhase>,

    cursor: Cell<(u16, u16)>,
    pub model_label: String,
    /// Cancel token for the in-flight agent turn (YYC-105). Held outside
    /// the agent mutex so the Ctrl+C handler can fire it without
    /// blocking on the lock that the prompt task is holding for the
    /// duration of the stream. `None` when no turn is in flight.
    pub current_turn_cancel: Option<tokio_util::sync::CancellationToken>,
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
            prompt_editor: PromptEditor::default(),
            thinking: false,
            scroll: 0,
            at_bottom: true,
            chat_max_scroll: Cell::new(0),
            slash_menu_selection: 0,
            prompt_mode: PromptMode::Insert,
            diff_style: DiffStyle::default(),
            queue: VecDeque::new(),
            deferred_queue: VecDeque::new(),
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
            orchestration_store: None,

            audit_log: None,
            frontend: super::frontend::TuiFrontend::default(),
            status_widgets: BTreeMap::new(),
            ui_runtime: UiRuntime::default(),
            active_ticks: Vec::new(),
            activity_throbber: ThrobberState::default(),
            effects: TuiEffects::default(),
            chat_clear_phase: Cell::new(ChatClearPhase::Idle),

            cursor: Cell::new((0, 0)),
            model_label,
            provider_label: None,
            current_turn_cancel: None,
            prompt_tokens_total: 0,
            completion_tokens_total: 0,
            prompt_tokens_last: 0,
            token_max,
            chat_render_store: RefCell::new(super::chat_render::ChatRenderStore::default()),

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
            let buf = buf.lock();
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
        demo_tool_log()
    }

    pub fn mode_label(&self) -> &'static str {
        match self.prompt_mode {
            PromptMode::Insert => self.prompt_editor.mode().badge(),
            _ => self.prompt_mode.badge(),
        }
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
        self.prompt_mode = if self.has_pause_prompt() {
            PromptMode::Ask
        } else if self.thinking {
            PromptMode::Busy
        } else if self.input.starts_with('/') {
            PromptMode::Command
        } else {
            PromptMode::Insert
        };
    }

    pub fn prompt_set(&mut self, text: impl Into<String>) {
        self.input = text.into();
        self.prompt_editor.set_text(&self.input);
    }

    pub fn prompt_clear(&mut self) {
        self.input.clear();
        self.prompt_editor.clear();
    }

    pub fn prompt_insert_str(&mut self, text: &str) {
        self.prompt_editor.insert_str(text);
        self.input = self.prompt_editor.text();
    }

    pub fn prompt_handle_key(&mut self, key: TuiKeyEvent) -> bool {
        let changed = self.prompt_editor.handle_key(key);
        if changed {
            self.input = self.prompt_editor.text();
        }
        changed
    }

    pub fn prompt_enter_intent(&self) -> PromptEnterIntent {
        self.prompt_editor.enter_intent()
    }

    pub fn prompt_escape_intent(&self) -> PromptEscapeIntent {
        self.prompt_editor.escape_intent()
    }

    pub fn model_status(&self) -> String {
        // YYC-60: design spec is `{model} · {used:n} / {max:n}` with comma
        // grouping. `used` is the latest turn's prompt_tokens (current
        // context window size); the bar represents context capacity, not
        // lifetime cost.
        // YYC-96: when running on a named provider profile, prefix the
        // string with `{profile} · ` so the user can tell which provider
        // any given turn will hit.
        let base = match &self.provider_label {
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
        };
        let widget_status = self.status_widget_summary();
        if widget_status.is_empty() {
            base
        } else {
            format!("{base} · {widget_status}")
        }
    }

    pub fn apply_widget_updates(&mut self, updates: Vec<vulcan_frontend_api::WidgetUpdate>) {
        for update in updates {
            match update.content {
                Some(content) => {
                    self.status_widgets.insert(update.id, content);
                }
                None => {
                    self.status_widgets.remove(&update.id);
                }
            }
        }
    }

    pub fn status_widgets(&self) -> Vec<(String, vulcan_frontend_api::WidgetContent)> {
        self.status_widgets
            .iter()
            .map(|(id, content)| (id.clone(), content.clone()))
            .collect()
    }

    fn status_widget_summary(&self) -> String {
        self.status_widgets
            .values()
            .map(|content| match content {
                vulcan_frontend_api::WidgetContent::Text(text) => text.clone(),
                vulcan_frontend_api::WidgetContent::Spinner(label) => label.clone(),
                vulcan_frontend_api::WidgetContent::Progress { label, ratio } => {
                    format!("{label} {:.0}%", ratio * 100.0)
                }
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }

    pub fn install_canvas_request(&mut self, request: vulcan_frontend_api::CanvasRequest) {
        self.ui_runtime.mount_canvas(request);
    }

    pub fn open_frontend_surface(&mut self, surface: vulcan_frontend_api::FrontendSurface) {
        self.ui_runtime.open_text_surface(surface);
    }

    pub fn install_tick_request(&mut self, request: vulcan_frontend_api::TickRequest) {
        self.active_ticks.push(ActiveTick {
            rate: request.rate,
            handle: request.handle,
            callback: request.callback,
            next_fire: Instant::now() + Duration::from_millis(request.rate.millis()),
        });
    }

    pub fn active_canvas_frame(&self) -> Option<vulcan_frontend_api::CanvasFrame> {
        self.ui_runtime.active_canvas_frame()
    }

    pub fn active_surface_frame(&self) -> Option<SurfaceFrame> {
        self.ui_runtime.active_frame()
    }

    pub fn open_session_picker(&mut self, selection: usize) {
        self.ui_runtime.open_session_picker(selection);
    }

    pub fn has_text_surface(&self) -> bool {
        self.ui_runtime.has_text_surface()
    }

    pub fn close_text_surface(&mut self) -> bool {
        self.ui_runtime.close_text_surface()
    }

    pub fn has_session_picker(&self) -> bool {
        self.ui_runtime.has_session_picker()
    }

    pub fn session_picker_selection(&self) -> usize {
        self.ui_runtime.session_picker_selection()
    }

    pub fn session_picker_up(&mut self) {
        self.ui_runtime.session_picker_up();
    }

    pub fn session_picker_down(&mut self) {
        let max = self.sessions.len().saturating_sub(1);
        self.ui_runtime.session_picker_down(max);
    }

    pub fn close_session_picker(&mut self) -> bool {
        self.ui_runtime.close_session_picker()
    }

    pub fn open_model_picker(&mut self, state: ModelPickerState) {
        self.ui_runtime.open_model_picker(state);
    }

    pub fn has_model_picker(&self) -> bool {
        self.ui_runtime.has_model_picker()
    }

    pub fn model_picker_state(&self) -> Option<&ModelPickerState> {
        self.ui_runtime.model_picker_state()
    }

    pub fn handle_model_picker_key(&mut self, key: TuiKeyEvent) -> ModelPickerOutcome {
        self.ui_runtime.handle_model_picker_key(key)
    }

    pub fn close_model_picker(&mut self) -> bool {
        self.ui_runtime.close_model_picker()
    }

    pub fn open_diff_scrubber(
        &mut self,
        path: String,
        hunks: Vec<crate::pause::DiffScrubHunk>,
        pause: crate::pause::AgentPause,
    ) {
        self.ui_runtime.open_diff_scrubber(path, hunks, pause);
    }

    pub fn has_diff_scrubber(&self) -> bool {
        self.ui_runtime.has_diff_scrubber()
    }

    pub fn diff_scrubber_state(&self) -> Option<&DiffScrubberState> {
        self.ui_runtime.diff_scrubber_state()
    }

    pub fn handle_diff_scrubber_key(&mut self, key: TuiKeyEvent) -> DiffScrubberOutcome {
        self.ui_runtime.handle_diff_scrubber_key(key)
    }

    pub fn close_diff_scrubber(&mut self) -> Option<crate::pause::AgentPause> {
        self.ui_runtime.close_diff_scrubber()
    }

    pub fn open_pause_prompt(&mut self, summary: String, pause: crate::pause::AgentPause) {
        self.ui_runtime.open_pause_prompt(summary, pause);
    }

    pub fn has_pause_prompt(&self) -> bool {
        self.ui_runtime.has_pause_prompt()
    }

    pub fn pause_prompt_state(&self) -> Option<&PausePromptState> {
        self.ui_runtime.pause_prompt_state()
    }

    pub fn handle_pause_prompt_key(&mut self, key: TuiKeyEvent) -> PausePromptOutcome {
        self.ui_runtime.handle_pause_prompt_key(key)
    }

    pub fn close_pause_prompt(&mut self) -> Option<crate::pause::AgentPause> {
        self.ui_runtime.close_pause_prompt()
    }

    pub fn has_active_canvas(&self) -> bool {
        self.ui_runtime.has_canvas()
    }

    pub fn open_provider_picker(&mut self, items: Vec<ProviderPickerEntry>, selection: usize) {
        self.ui_runtime.open_provider_picker(items, selection);
    }

    pub fn has_provider_picker(&self) -> bool {
        self.ui_runtime.has_provider_picker()
    }

    pub fn provider_picker_up(&mut self) {
        self.ui_runtime.provider_picker_up();
    }

    pub fn provider_picker_down(&mut self) {
        self.ui_runtime.provider_picker_down();
    }

    pub fn selected_provider(&self) -> Option<ProviderPickerEntry> {
        self.ui_runtime.selected_provider()
    }

    pub fn handle_provider_picker_key(&mut self, key: TuiKeyEvent) -> ProviderPickerOutcome {
        self.ui_runtime.handle_provider_picker_key(key)
    }

    pub fn close_provider_picker(&mut self) -> bool {
        self.ui_runtime.close_provider_picker()
    }

    pub fn handle_canvas_key(&mut self, key: vulcan_frontend_api::CanvasKey) -> bool {
        self.ui_runtime.handle_canvas_key(key)
    }

    pub fn pump_frontend_ticks(&mut self) {
        let now = Instant::now();
        for tick in &mut self.active_ticks {
            if tick.handle.is_stopped() {
                continue;
            }
            if now >= tick.next_fire {
                (tick.callback)(&tick.handle);
                tick.next_fire = now + Duration::from_millis(tick.rate.millis());
            }
        }
        self.active_ticks.retain(|tick| !tick.handle.is_stopped());
        self.ui_runtime.handle_tick();
    }

    pub fn activity_motion_active(&self) -> bool {
        self.thinking
            || !self.queue.is_empty()
            || !self.deferred_queue.is_empty()
            || self
                .status_widgets
                .values()
                .any(|content| !matches!(content, vulcan_frontend_api::WidgetContent::Text(_)))
            || self.chat_clear_phase.get() != ChatClearPhase::Idle
            || self.effects.chat_running()
            || self.effects.model_picker_running()
            || self.messages.iter().any(|message| {
                message.segments.iter().any(|segment| {
                    matches!(
                        segment,
                        MessageSegment::ToolCall {
                            status: ToolStatus::InProgress,
                            ..
                        }
                    )
                })
            })
    }

    pub fn advance_activity_motion(&mut self) {
        if self.activity_motion_active() {
            self.activity_throbber.calc_next();
            self.effects.advance_prompt_border_sweep();
        }
    }

    pub fn request_chat_clear(&self) {
        self.chat_clear_phase.set(ChatClearPhase::Requested);
    }

    pub fn start_chat_clear_effect_if_pending(&self, area: ratatui::layout::Rect) {
        if self.effects.chat_running() {
            return;
        }
        match self.chat_clear_phase.get() {
            ChatClearPhase::Requested => {
                self.effects.trigger_chat_clear(area);
                self.chat_clear_phase.set(ChatClearPhase::Exploding);
            }
            ChatClearPhase::RevealRequested => {
                self.effects.trigger_chat_reveal(area);
                self.chat_clear_phase.set(ChatClearPhase::Revealing);
            }
            ChatClearPhase::Idle | ChatClearPhase::Exploding | ChatClearPhase::Revealing => {}
        }
    }

    pub fn finish_chat_clear_if_idle(&mut self) -> bool {
        if self.effects.chat_running() {
            return false;
        }
        match self.chat_clear_phase.get() {
            ChatClearPhase::Exploding => {
                self.messages.clear();
                self.chat_render_store.borrow_mut().clear();
                self.chat_clear_phase.set(ChatClearPhase::RevealRequested);
                true
            }
            ChatClearPhase::Revealing => {
                self.chat_clear_phase.set(ChatClearPhase::Idle);
                true
            }
            ChatClearPhase::Idle | ChatClearPhase::Requested | ChatClearPhase::RevealRequested => {
                false
            }
        }
    }

    pub fn cancel_stack(&self) -> Vec<CancelScope> {
        let mut stack = Vec::new();
        if self.thinking {
            stack.push(CancelScope::Turn);
        }
        if self.has_pause_prompt() || self.has_diff_scrubber() || self.has_text_surface() {
            stack.push(CancelScope::Dialog);
        }
        if self.ui_runtime.has_canvas() {
            stack.push(CancelScope::Canvas);
        }
        stack
    }

    pub fn pop_cancel_scope(&mut self) -> CancelPop {
        match self.cancel_stack().pop() {
            Some(CancelScope::Canvas) => {
                self.ui_runtime.exit_canvas();
                CancelPop::Popped(CancelScope::Canvas)
            }
            Some(CancelScope::Dialog) => {
                if let Some(pause) = self.close_pause_prompt() {
                    let _ = pause.reply.send(crate::pause::AgentResume::Deny);
                }
                if let Some(pause) = self.close_diff_scrubber() {
                    let _ = pause.reply.send(crate::pause::AgentResume::Deny);
                }
                self.close_text_surface();
                CancelPop::Popped(CancelScope::Dialog)
            }
            Some(CancelScope::Turn) => CancelPop::CancelTurn,
            None => CancelPop::None,
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
        sink.latest()
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
        // Build the orchestrator's own tile from current phase /
        // task / events.
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
        let mut tiles = vec![SubAgentTile {
            name: "main".into(),
            role: "orchestrator".into(),
            state: self.orchestration.phase.label().into(),
            color: self.orchestration.phase.color(),
            log,
            cpu: Vec::new(),
        }];

        // YYC-207: append a tile per recent child run from the
        // orchestration store, when wired. Newest first so active
        // children show up at the top of the mesh.
        if let Some(store) = &self.orchestration_store {
            for record in store.recent(8) {
                tiles.push(child_tile_from(&record));
            }
        }
        tiles
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
        // YYC-207: append child runs as depth-1 nodes under root.
        // Active (non-terminal) records highlight; terminal ones
        // surface their final status symbol.
        if let Some(store) = &self.orchestration_store {
            for record in store.recent(8) {
                nodes.push(child_tree_node(&record));
            }
        }
        nodes
    }

    pub fn delegated_worker_count(&self) -> usize {
        // YYC-207: live child count — non-terminal records only.
        match &self.orchestration_store {
            Some(store) => store
                .list()
                .iter()
                .filter(|r| !r.status.is_terminal())
                .count(),
            None => 0,
        }
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

/// YYC-207: project a `ChildAgentRecord` into the SubAgentTile
/// shape the TiledMesh view renders.
fn child_tile_from(record: &crate::orchestration::ChildAgentRecord) -> SubAgentTile {
    use crate::orchestration::ChildStatus;
    let (state_label, color) = match record.status {
        ChildStatus::Pending => ("pending", Palette::MUTED),
        ChildStatus::Running => ("running", Palette::BLUE),
        ChildStatus::Blocked => ("blocked", Palette::YELLOW),
        ChildStatus::Completed => ("done", Palette::GREEN),
        ChildStatus::Failed => ("error", Palette::RED),
        ChildStatus::Cancelled => ("cancelled", Palette::MUTED),
    };
    let mut log = Vec::new();
    log.push(short_text(&record.task_summary, 60).to_string());
    if let Some(phase) = &record.current_phase {
        log.push(format!("phase · {phase}"));
    }
    log.push(format!(
        "budget · {}/{}",
        record.iterations_used, record.max_iterations
    ));
    if let Some(err) = &record.error {
        log.push(format!("error · {}", short_text(err, 48)));
    } else if let Some(summary) = &record.final_summary {
        log.push(format!("summary · {}", short_text(summary, 48)));
    }
    let id_short: String = record.id.to_string().chars().take(8).collect();
    SubAgentTile {
        name: format!("child:{id_short}"),
        role: "subagent".into(),
        state: state_label.into(),
        color,
        log,
        cpu: Vec::new(),
    }
}

/// YYC-207: project a `ChildAgentRecord` into a TreeNode for the
/// TreeOfThought view. Non-terminal records are marked active so
/// the tree highlights the live frontier.
fn child_tree_node(record: &crate::orchestration::ChildAgentRecord) -> TreeNode {
    use crate::orchestration::ChildStatus;
    let (symbol, color) = match record.status {
        ChildStatus::Pending => ("○", Palette::MUTED),
        ChildStatus::Running => ("●", Palette::BLUE),
        ChildStatus::Blocked => ("⏸", Palette::YELLOW),
        ChildStatus::Completed => ("✓", Palette::GREEN),
        ChildStatus::Failed => ("✗", Palette::RED),
        ChildStatus::Cancelled => ("⊘", Palette::MUTED),
    };
    let id_short: String = record.id.to_string().chars().take(8).collect();
    TreeNode {
        depth: 1,
        label: format!(
            "└─ child:{id_short} · {}",
            short_text(&record.task_summary, 40)
        ),
        state: symbol.into(),
        color,
        active: !record.status.is_terminal(),
    }
}

#[cfg(test)]
mod tests;
