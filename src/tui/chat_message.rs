//! Chat-message domain types extracted from `tui/state.rs` (YYC-110).
//!
//! `ChatMessage` is the per-turn structure the renderer reads, and
//! `MessageSegment` captures the natural interleaving of reasoning,
//! tool calls, and text chunks as they arrive. They live here together
//! because every method on `ChatMessage` mutates `segments`.

use super::state::{ChatRole, ToolStatus};

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
        details: Option<serde_json::Value>,
        custom_lines: Option<Vec<String>>,
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
            details: None,
            custom_lines: None,
            elided_lines: 0,
            elapsed_ms: None,
        });
        self.bump_render_version();
    }

    /// Mark the most recent in-progress ToolCall with this name as done.
    /// Walks segments in reverse so concurrent dispatch (YYC-34) still
    /// pairs each end with its own start as the matching tail.
    pub fn finish_tool(&mut self, name: &str, ok: bool) {
        self.finish_tool_with(name, ok, None, None, 0, None, None, None);
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
        details: Option<serde_json::Value>,
        custom_lines: Option<Vec<String>>,
    ) {
        for seg in self.segments.iter_mut().rev() {
            if let MessageSegment::ToolCall {
                name: n,
                status,
                output_preview: op,
                result_meta: rm,
                details: d,
                custom_lines: cl,
                elided_lines: el,
                elapsed_ms: em,
                ..
            } = seg
                && n == name
                && matches!(status, ToolStatus::InProgress)
            {
                *status = ToolStatus::Done(ok);
                *op = output_preview;
                *rm = result_meta;
                *d = details;
                *cl = custom_lines;
                *el = elided_lines;
                *em = elapsed_ms;
                self.bump_render_version();
                return;
            }
        }
    }
}
