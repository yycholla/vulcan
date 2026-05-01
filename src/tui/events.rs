//! Stream-event handling extracted from `tui/mod.rs` (YYC-108).
//!
//! Owns the per-event state mutations (handle_stream_event), the
//! per-frame draw budget (render_wake_for_stream_batch), the
//! "force a redraw" classifier for terminal events, and the two
//! agent-launch helpers (submit_prompt, refresh_sessions) that the
//! Done branch and the Enter handler share.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, mpsc};

use crate::agent::Agent;
use crate::provider::StreamEvent;

use super::state::{AppState, ChatMessage, ChatRole};

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

/// Spawn a fresh agent turn for `msg`. Updates chat state (User + empty
/// Agent message), flips thinking on, re-engages auto-follow, then spawns
/// `run_prompt_stream` against the agent. Used by the Enter handler for
/// new submissions and by the Done handler when draining the queue
/// (YYC-61).
pub(super) fn submit_prompt(
    app: &mut AppState,
    agent: &Arc<Mutex<Agent>>,
    stream_tx: &mpsc::Sender<StreamEvent>,
    msg: String,
) {
    app.messages.push(ChatMessage {
        role: ChatRole::User,
        content: msg.clone(),
        ..Default::default()
    });
    app.messages.push(ChatMessage {
        role: ChatRole::Agent,
        content: String::new(),
        ..Default::default()
    });
    app.thinking = true;
    app.at_bottom = true;
    app.note_prompt_submitted(&msg);

    // YYC-105: hold a cancel token outside the agent mutex so Ctrl+C
    // can fire it without waiting for the prompt task to release the
    // lock. The agent mirrors the same token internally for tools /
    // hooks that still consult `self.turn_cancel`.
    let cancel = tokio_util::sync::CancellationToken::new();
    app.current_turn_cancel = Some(cancel.clone());
    let tx = stream_tx.clone();
    let a = agent.clone();
    tokio::spawn(async move {
        let mut a = a.lock().await;
        let _ = a.run_prompt_stream_with_cancel(&msg, tx, cancel).await;
    });
}

pub(super) async fn refresh_sessions(agent: &Arc<Mutex<Agent>>, app: &mut AppState) {
    let (summaries, active_session_id) = {
        let a = agent.lock().await;
        (
            a.memory().list_sessions(12).unwrap_or_default(),
            a.session_id().to_string(),
        )
    };
    app.hydrate_sessions(&summaries, &active_session_id);
}

pub(super) async fn handle_stream_event(
    app: &mut AppState,
    agent: &Arc<Mutex<Agent>>,
    stream_tx: &mpsc::Sender<StreamEvent>,
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
                // Strip leading whitespace so models that emit `\n\n`
                // preambles don't render gaps before the visible body
                // when the renderer falls back to `content`.
                if last.content.is_empty() {
                    let trimmed = chunk.trim_start_matches(|c: char| {
                        c == '\n' || c == '\r' || c == ' ' || c == '\t'
                    });
                    last.content.push_str(trimmed);
                } else {
                    last.content.push_str(&chunk);
                }
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
            app.current_turn_cancel = None;
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
            refresh_sessions(agent, app).await;
            // YYC-125: drain ALL queued steers at turn end as a single
            // batched prompt. Multiple mid-turn submissions land as one
            // combined user message rather than dripping one prompt per
            // turn. /queue deferrals (deferred_queue) drain strictly
            // after the steer batch — and only one per Done — so user
            // intent is preserved.
            if !app.queue.is_empty() {
                let parts: Vec<String> = app.queue.drain(..).collect();
                let batched = parts.join("\n\n");
                submit_prompt(app, agent, stream_tx, batched);
            } else if let Some(next) = app.deferred_queue.pop_front() {
                submit_prompt(app, agent, stream_tx, next);
            }
        }
        StreamEvent::Error(e) => {
            if let Some(last) = app.messages.last_mut()
                && last.content.is_empty()
            {
                last.set_content(format!("⚠ Error: {e}"));
            }
            app.thinking = false;
            app.current_turn_cancel = None;
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
            details,
            result_meta,
            elided_lines,
            elapsed_ms,
            ..
        } => {
            let custom_lines = app.frontend.render_tool_result(
                &name,
                ok,
                output_preview.as_deref(),
                details.as_ref(),
            );
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
                    details,
                    custom_lines,
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
}
