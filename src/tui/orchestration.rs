//! Orchestration / multi-agent tile state extracted from `tui/state.rs`
//! (YYC-110). These types describe the (currently demo-data-shaped)
//! orchestration view; the runtime they need is tracked separately
//! under YYC-68.

use ratatui::style::Color;

use super::theme::Palette;

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
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Thinking => "thinking",
            Self::ToolRunning => "tool",
            Self::Paused => "paused",
            Self::Complete => "done",
            Self::Error => "error",
        }
    }

    pub(crate) fn color(self) -> Color {
        match self {
            Self::Idle => Palette::MUTED,
            Self::Thinking => Palette::YELLOW,
            Self::ToolRunning => Palette::BLUE,
            Self::Paused => Palette::RED,
            Self::Complete => Palette::GREEN,
            Self::Error => Palette::RED,
        }
    }

    pub(crate) fn symbol(self) -> &'static str {
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
