//! Stream-event handling extracted from `tui/mod.rs` (YYC-108).
//! `handle_stream_event` is the single point where provider stream
//! events get applied to `AppState`; the small `RenderWake` helper
//! decides whether the main loop should redraw immediately or wait
//! for the next frame budget.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, mpsc};

use crate::agent::Agent;
use crate::provider::StreamEvent;

use super::state::{AppState, ChatMessage, ChatRole};

/// Frame-pacing budget for streaming text. Dropped events still update
/// `AppState`; they just don't force a redraw.
pub(super) const STREAM_FRAME_BUDGET: Duration = Duration::from_millis(16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RenderWake {
    Now,
    Wait(Duration),
}

pub(super) fn render_wake_for_stream_batch(
    last_draw: Instant,
    now: Instant,
    is_terminal_event: bool,
) -> RenderWake {
    if is_terminal_event {
        return RenderWake::Now;
    }

    let elapsed = now.saturating_duration_since(last_draw);
    if elapsed >= STREAM_FRAME_BUDGET {
        RenderWake::Now
    } else {
        RenderWake::Wait(STREAM_FRAME_BUDGET - elapsed)
    }
}

pub(super) fn stream_event_forces_redraw(ev: &StreamEvent) -> bool {
    matches!(ev, StreamEvent::Done(_) | StreamEvent::Error(_))
}

pub(super) async fn handle_stream_event(
    app: &mut AppState,
    agent: &Arc<Mutex<Agent>>,
    stream_tx: &mpsc::UnboundedSender<StreamEvent>,
    ev: StreamEvent,
) {
    match ev {
        StreamEvent::Text(chunk) => {
            if let Some(last) = app.messages.last_mut()
                && matches!(last.role, ChatRole::Agent)
            {
                // Append to both the segment timeline (the YYC-71
                // ordered renderer) and the legacy `content` field
                // (kept so other code that peeks at .content keeps working).
                last.append_text(&chunk);
                last.content.push_str(&chunk);
            }
        }
        StreamEvent::Reasoning(chunk) => {
            // Per-token reasoning trace from thinking-mode models. Push to
            // the segment timeline so it interleaves with tool calls in
            // render order; also append to the legacy `reasoning` field so
            // latest_reasoning() etc. continue to work.
            if let Some(last) = app.messages.last_mut()
                && matches!(last.role, ChatRole::Agent)
            {
                last.append_reasoning(&chunk);
                last.reasoning.push_str(&chunk);
            }
            app.note_reasoning();
        }
        StreamEvent::Done(resp) => {
            app.thinking = false;
            if let Some(usage) = resp.usage {
                // YYC-60: track lifetime totals for cost (YYC-67) and the
                // latest prompt size for the in-status capacity bar.
                app.prompt_tokens_total = app
                    .prompt_tokens_total
                    .saturating_add(usage.prompt_tokens as u32);
                app.completion_tokens_total = app
                    .completion_tokens_total
                    .saturating_add(usage.completion_tokens as u32);
                app.prompt_tokens_last = usage.prompt_tokens as u32;
            }
            app.note_done();
            super::refresh_sessions(agent, app).await;
            // YYC-61: drain one queued prompt per turn end. Subsequent queued
            // prompts ride the next Done event in the same way.
            if let Some(next) = app.queue.pop_front() {
                super::submit_prompt(app, agent, stream_tx, next);
            }
        }
        StreamEvent::Error(e) => {
            if let Some(last) = app.messages.last_mut()
                && last.content.is_empty()
            {
                last.set_content(format!("⚠ Error: {e}"));
            }
            app.thinking = false;
            // YYC-67: record provider-level error for telemetry.
            app.provider_errors_total = app.provider_errors_total.saturating_add(1);
            app.note_error(&e);
        }
        StreamEvent::ToolCallStart {
            name, args_summary, ..
        } => {
            // YYC-71: push the tool-call segment into the timeline
            // (interleaved with reasoning/text). YYC-74: carry the args
            // summary so the card has structured context.
            if let Some(last) = app.messages.last_mut()
                && matches!(last.role, ChatRole::Agent)
            {
                last.push_tool_start_with(name.clone(), args_summary);
            }
            app.note_tool_start(&name);
        }
        StreamEvent::ToolCallEnd {
            name,
            ok,
            output_preview,
            result_meta,
            elided_lines,
            elapsed_ms,
            ..
        } => {
            if let Some(last) = app.messages.last_mut()
                && matches!(last.role, ChatRole::Agent)
            {
                // YYC-74: stamp preview + meta + timing onto the matching
                // segment for the card. YYC-78: stash elided count for the
                // collapse footer.
                last.finish_tool_with(
                    &name,
                    ok,
                    output_preview,
                    result_meta,
                    elided_lines,
                    Some(elapsed_ms),
                );
            }
            // YYC-67: tool call telemetry.
            app.tool_calls_total = app.tool_calls_total.saturating_add(1);
            if !ok {
                app.tool_errors_total = app.tool_errors_total.saturating_add(1);
            }
            app.note_tool_end(&name, ok);
        }
    }
    // Suppress unused import on ChatMessage when only role check matters.
    let _: fn(&ChatMessage) = |_| {};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_batching_caps_stream_redraws_to_frame_budget() {
        let start = Instant::now();

        assert_eq!(
            render_wake_for_stream_batch(start, start + Duration::from_millis(1), false),
            RenderWake::Wait(Duration::from_millis(15))
        );
    }

    #[test]
    fn input_events_render_immediately() {
        let start = Instant::now();

        assert_eq!(
            render_wake_for_stream_batch(start, start + Duration::from_millis(1), true),
            RenderWake::Now
        );
    }
}
