use super::Agent;
use crate::provider::{ChatResponse, StreamEvent};

/// Provider-call mode for one [`TurnRunnerMut::run`] invocation.
///
/// Buffered mode calls `provider.chat`, gets the full response in one shot,
/// and emits a single [`TurnEvent::Text`] for the assistant content.
/// Streaming mode calls `provider.chat_stream`, fan-outs incremental
/// [`TurnEvent::Text`]/`Reasoning` events, and assembles the final
/// [`ChatResponse`] from the stream's `Done` frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::agent) enum TurnMode {
    Buffered,
    Streaming,
}

/// Terminal status for a unified turn execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::agent) enum TurnStatus {
    Completed,
    Cancelled,
    MaxIterations,
}

/// Result of a unified turn execution.
///
/// `final_text` is what the buffered caller would return to the user,
/// `final_response` is the assembled [`ChatResponse`] the streaming
/// adapter forwards as `StreamEvent::Done`, and `status` lets the
/// adapter pick the right finish_reason / persistence path without
/// re-deriving it from `final_text`.
pub(in crate::agent) struct TurnOutcome {
    pub final_text: String,
    pub final_response: Option<ChatResponse>,
    pub status: TurnStatus,
}

/// Session-local turn execution facade.
///
/// Slice 1 keeps existing `Agent` methods as adapters, but new turn execution
/// behavior should move behind this seam instead of growing parallel buffered
/// and streaming paths.
#[allow(dead_code)]
pub(in crate::agent) struct TurnRunner<'a> {
    pub(in crate::agent) agent: &'a Agent,
}

impl<'a> TurnRunner<'a> {
    #[allow(dead_code)]
    pub(in crate::agent) fn new(agent: &'a Agent) -> Self {
        Self { agent }
    }
}

pub(in crate::agent) struct TurnRunnerMut<'a> {
    pub(in crate::agent) agent: &'a mut Agent,
}

impl<'a> TurnRunnerMut<'a> {
    pub(in crate::agent) fn new(agent: &'a mut Agent) -> Self {
        Self { agent }
    }
}

/// Domain-level event emitted while a turn is running.
///
/// Provider streaming events and daemon/frontend frames are adapters around
/// this vocabulary; turn execution should not depend on either wire shape.
#[derive(Debug, Clone)]
pub(in crate::agent) enum TurnEvent {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ToolCallStart {
        id: String,
        name: String,
        args_summary: Option<String>,
    },
    ToolCallEnd {
        id: String,
        name: String,
        ok: bool,
        output_preview: Option<String>,
        details: Option<serde_json::Value>,
        result_meta: Option<String>,
        elided_lines: usize,
        elapsed_ms: u64,
    },
    Compacted {
        earlier_messages: usize,
    },
    CompactionForced {
        extension_id: String,
        reason: String,
    },
    ProviderDone {
        response: ChatResponse,
    },
    Error {
        message: String,
    },
}

impl From<StreamEvent> for TurnEvent {
    fn from(event: StreamEvent) -> Self {
        match event {
            StreamEvent::Text(text) => Self::Text { text },
            StreamEvent::Reasoning(text) => Self::Reasoning { text },
            StreamEvent::ToolCallStart {
                id,
                name,
                args_summary,
            } => Self::ToolCallStart {
                id,
                name,
                args_summary,
            },
            StreamEvent::ToolCallEnd {
                id,
                name,
                ok,
                output_preview,
                details,
                result_meta,
                elided_lines,
                elapsed_ms,
            } => Self::ToolCallEnd {
                id,
                name,
                ok,
                output_preview,
                details,
                result_meta,
                elided_lines,
                elapsed_ms,
            },
            StreamEvent::Done(response) => Self::ProviderDone { response },
            StreamEvent::Error(message) => Self::Error { message },
        }
    }
}

impl From<TurnEvent> for StreamEvent {
    fn from(event: TurnEvent) -> Self {
        match event {
            TurnEvent::Text { text } => Self::Text(text),
            TurnEvent::Reasoning { text } => Self::Reasoning(text),
            TurnEvent::ToolCallStart {
                id,
                name,
                args_summary,
            } => Self::ToolCallStart {
                id,
                name,
                args_summary,
            },
            TurnEvent::ToolCallEnd {
                id,
                name,
                ok,
                output_preview,
                details,
                result_meta,
                elided_lines,
                elapsed_ms,
            } => Self::ToolCallEnd {
                id,
                name,
                ok,
                output_preview,
                details,
                result_meta,
                elided_lines,
                elapsed_ms,
            },
            TurnEvent::Compacted { earlier_messages } => Self::Text(format!(
                "_(compacted {earlier_messages} earlier messages into a summary to fit context)_\n"
            )),
            TurnEvent::CompactionForced {
                extension_id,
                reason,
            } => Self::Text(format!(
                "_(compaction forced despite {extension_id} veto: {reason})_\n"
            )),
            TurnEvent::ProviderDone { response } => Self::Done(response),
            TurnEvent::Error { message } => Self::Error(message),
        }
    }
}
