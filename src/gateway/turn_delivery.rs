//! Gateway inbound Turn delivery.
//!
//! Owns the inbound-row to outbound-result workflow: slash command
//! shortcut, daemon Session routing, prompt streaming, atomic success
//! completion, and failure marking.

use std::sync::Arc;

use crate::daemon::protocol::StreamFrame;
use crate::gateway::commands::{CommandDispatcher, DispatchCtx};
use crate::gateway::daemon_client::GatewayDaemonClient;
use crate::gateway::lane::LaneKey;
use crate::gateway::lane_router::DaemonLaneRouter;
use crate::gateway::queue::{InboundQueue, InboundRow, OutboundQueue};
use crate::gateway::render_registry::{RenderKey, RenderRegistry};
use crate::gateway::stream_render::StreamRenderer;
use crate::platform::{OutboundMessage, PlatformCapabilities};
use crate::provider::{ChatResponse, StreamEvent};

pub(crate) struct TurnDelivery<'a> {
    lane_router: &'a DaemonLaneRouter,
    daemon_client: &'a GatewayDaemonClient,
    inbound_queue: &'a InboundQueue,
    outbound_queue: &'a Arc<OutboundQueue>,
    render_registry: &'a Arc<RenderRegistry>,
    platform_caps: PlatformCapabilities,
    commands: &'a CommandDispatcher,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryOutcome {
    CommandShortcut,
    DaemonPrompt,
}

impl<'a> TurnDelivery<'a> {
    pub(crate) fn new(
        lane_router: &'a DaemonLaneRouter,
        daemon_client: &'a GatewayDaemonClient,
        inbound_queue: &'a InboundQueue,
        outbound_queue: &'a Arc<OutboundQueue>,
        render_registry: &'a Arc<RenderRegistry>,
        platform_caps: PlatformCapabilities,
        commands: &'a CommandDispatcher,
    ) -> Self {
        Self {
            lane_router,
            daemon_client,
            inbound_queue,
            outbound_queue,
            render_registry,
            platform_caps,
            commands,
        }
    }

    pub(crate) async fn deliver(&self, row: InboundRow) -> anyhow::Result<DeliveryOutcome> {
        let lane = LaneKey {
            platform: row.platform.clone(),
            chat_id: row.chat_id.clone(),
        };

        match self.dispatch_command(&row, &lane).await {
            Ok(Some(reply)) => {
                self.complete(row, reply).await?;
                Ok(DeliveryOutcome::CommandShortcut)
            }
            Ok(None) => self.deliver_prompt(row, &lane).await,
            Err(e) => {
                self.mark_failed(row.id, &e).await?;
                Err(e)
            }
        }
    }

    async fn dispatch_command(
        &self,
        row: &InboundRow,
        lane: &LaneKey,
    ) -> anyhow::Result<Option<String>> {
        self.commands
            .dispatch(
                &row.text,
                DispatchCtx {
                    lane,
                    user_id: &row.user_id,
                    lane_router: self.lane_router,
                    daemon_client: self.daemon_client,
                    body: "",
                },
            )
            .await
    }

    async fn deliver_prompt(
        &self,
        row: InboundRow,
        lane: &LaneKey,
    ) -> anyhow::Result<DeliveryOutcome> {
        match self.run_prompt_via_daemon(lane, &row.text).await {
            Ok(delivery) => {
                if delivery.rendered_streaming {
                    self.inbound_queue.mark_done(row.id).await?;
                } else {
                    self.complete(row, delivery.reply_text).await?;
                }
                Ok(DeliveryOutcome::DaemonPrompt)
            }
            Err(e) => {
                self.mark_failed(row.id, &e).await?;
                Err(e)
            }
        }
    }

    async fn complete(&self, row: InboundRow, text: String) -> anyhow::Result<i64> {
        self.inbound_queue
            .complete_with_outbound(
                row.id,
                OutboundMessage {
                    platform: row.platform,
                    chat_id: row.chat_id,
                    text,
                    attachments: vec![],
                    reply_to: None,
                    edit_target: None,
                    turn_id: None,
                },
            )
            .await
    }

    async fn mark_failed(&self, id: i64, error: &anyhow::Error) -> anyhow::Result<()> {
        self.inbound_queue.mark_failed(id, &error.to_string()).await
    }

    /// Ensure the daemon session exists for `lane` and stream a
    /// `prompt.stream` request through the gateway-owned shared client.
    /// Drains stream frames through the gateway render adapter. The
    /// final response's `text` field (if any) takes precedence over
    /// accumulated text chunks so buffered replies don't double-emit.
    async fn run_prompt_via_daemon(
        &self,
        lane: &LaneKey,
        input: &str,
    ) -> anyhow::Result<PromptDelivery> {
        let client = self
            .daemon_client
            .shared_client()
            .await
            .map_err(|e| anyhow::anyhow!("client connect: {e}"))?;

        let session_id = self
            .lane_router
            .ensure_session(lane, &client)
            .await
            .map_err(|e| anyhow::anyhow!("ensure_session: {e}"))?;

        let mut stream = client
            .call_stream_at_session(
                &session_id,
                "prompt.stream",
                serde_json::json!({ "text": input }),
            )
            .await
            .map_err(|e| anyhow::anyhow!("prompt.stream call: {e}"))?;

        let mut renderer = GatewayTurnRenderer::new(
            lane,
            &session_id,
            self.platform_caps.clone(),
            Arc::clone(self.outbound_queue),
            Arc::clone(self.render_registry),
        );
        while let Some(frame) = stream.frames.recv().await {
            renderer.handle_frame(frame).await?;
        }

        let final_response = stream
            .done
            .await
            .map_err(|_| anyhow::anyhow!("daemon dropped completion sender"))?
            .map_err(|e| anyhow::anyhow!("stream completion: {e}"))?;

        if let Some(err) = final_response.error {
            anyhow::bail!("daemon prompt.stream error [{}]: {}", err.code, err.message);
        }

        let final_text = final_response
            .result
            .as_ref()
            .and_then(|result| result.get("text"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        renderer.finish(final_text).await
    }
}

struct PromptDelivery {
    reply_text: String,
    rendered_streaming: bool,
}

struct GatewayTurnRenderer {
    reply_text: String,
    stream_renderer: Option<StreamRenderer>,
    #[cfg(test)]
    non_text_frames: usize,
}

impl GatewayTurnRenderer {
    fn new(
        lane: &LaneKey,
        turn_id: &str,
        platform_caps: PlatformCapabilities,
        outbound_queue: Arc<OutboundQueue>,
        render_registry: Arc<RenderRegistry>,
    ) -> Self {
        let stream_renderer = if platform_caps.supports_edit {
            Some(StreamRenderer::new(
                RenderKey {
                    platform: lane.platform.clone(),
                    chat_id: lane.chat_id.clone(),
                    turn_id: turn_id.to_string(),
                },
                platform_caps.edit_min_interval_ms,
                outbound_queue,
                render_registry,
            ))
        } else {
            None
        };
        Self {
            reply_text: String::new(),
            stream_renderer,
            #[cfg(test)]
            non_text_frames: 0,
        }
    }

    async fn handle_frame(&mut self, frame: StreamFrame) -> anyhow::Result<()> {
        if frame.stream != "text" {
            #[cfg(test)]
            {
                self.non_text_frames += 1;
            }
        }
        let Some(event) = stream_frame_to_event(frame) else {
            return Ok(());
        };
        if let StreamEvent::Text(chunk) = &event {
            self.reply_text.push_str(chunk);
        }
        if let Some(renderer) = &mut self.stream_renderer {
            renderer.handle(event).await?;
        }
        Ok(())
    }

    async fn finish(mut self, final_text: Option<String>) -> anyhow::Result<PromptDelivery> {
        let reply_text = final_text.unwrap_or_else(|| self.reply_text.clone());
        let rendered_streaming = self.stream_renderer.is_some();
        if let Some(renderer) = &mut self.stream_renderer {
            if self.reply_text.is_empty() && !reply_text.is_empty() {
                renderer
                    .handle(StreamEvent::Text(reply_text.clone()))
                    .await?;
            }
            renderer
                .handle(StreamEvent::Done(ChatResponse {
                    content: Some(reply_text.clone()),
                    tool_calls: None,
                    usage: None,
                    finish_reason: None,
                    reasoning_content: None,
                }))
                .await?;
        }
        Ok(PromptDelivery {
            reply_text,
            rendered_streaming,
        })
    }

    #[cfg(test)]
    fn observed_non_text_frames(&self) -> usize {
        self.non_text_frames
    }
}

fn stream_frame_to_event(frame: StreamFrame) -> Option<StreamEvent> {
    match frame.stream.as_str() {
        "text" => frame
            .data
            .get("chunk")
            .and_then(|v| v.as_str())
            .map(|s| StreamEvent::Text(s.to_string())),
        "reasoning" => frame
            .data
            .get("chunk")
            .and_then(|v| v.as_str())
            .map(|s| StreamEvent::Reasoning(s.to_string())),
        "tool_call_start" => Some(StreamEvent::ToolCallStart {
            id: frame
                .data
                .get("tool_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            name: frame
                .data
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            args_summary: frame
                .data
                .get("args_summary")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        }),
        "tool_call_end" => Some(StreamEvent::ToolCallEnd {
            id: frame
                .data
                .get("tool_id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            name: frame
                .data
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            ok: frame
                .data
                .get("ok")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            output_preview: frame
                .data
                .get("output_preview")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            details: frame.data.get("details").cloned(),
            result_meta: frame
                .data
                .get("result_meta")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            elided_lines: 0,
            elapsed_ms: frame
                .data
                .get("elapsed_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        }),
        "error" => frame
            .data
            .get("reason")
            .and_then(|v| v.as_str())
            .map(|s| StreamEvent::Error(s.to_string())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::memory::in_memory_gateway_pool;

    fn renderer(caps: PlatformCapabilities) -> (GatewayTurnRenderer, Arc<OutboundQueue>) {
        let outbound = Arc::new(OutboundQueue::new(in_memory_gateway_pool().unwrap(), 5));
        let registry = Arc::new(RenderRegistry::new());
        let lane = LaneKey {
            platform: "loopback".into(),
            chat_id: "c".into(),
        };
        (
            GatewayTurnRenderer::new(&lane, "turn-1", caps, outbound.clone(), registry),
            outbound,
        )
    }

    #[tokio::test]
    async fn render_adapter_accumulates_text_and_observes_non_text_frames() {
        let (mut adapter, _outbound) = renderer(PlatformCapabilities::default());
        adapter
            .handle_frame(StreamFrame {
                version: 1,
                id: Some("req-1".into()),
                stream: "text".into(),
                data: serde_json::json!({ "chunk": "hello" }),
            })
            .await
            .unwrap();
        adapter
            .handle_frame(StreamFrame {
                version: 1,
                id: Some("req-1".into()),
                stream: "tool_call_start".into(),
                data: serde_json::json!({
                    "tool_id": "tool-1",
                    "name": "noop",
                    "args_summary": "arg"
                }),
            })
            .await
            .unwrap();

        assert_eq!(adapter.observed_non_text_frames(), 1);
        let delivery = adapter.finish(None).await.unwrap();
        assert_eq!(delivery.reply_text, "hello");
        assert!(!delivery.rendered_streaming);
    }

    #[tokio::test]
    async fn render_adapter_streams_when_platform_supports_edits() {
        let (mut adapter, outbound) = renderer(PlatformCapabilities {
            supports_edit: true,
            edit_min_interval_ms: 0,
            ..PlatformCapabilities::default()
        });
        adapter
            .handle_frame(StreamFrame {
                version: 1,
                id: Some("req-1".into()),
                stream: "text".into(),
                data: serde_json::json!({ "chunk": "hello" }),
            })
            .await
            .unwrap();
        let delivery = adapter.finish(None).await.unwrap();
        assert!(delivery.rendered_streaming);

        let row = outbound
            .claim_due(chrono::Utc::now().timestamp())
            .await
            .unwrap()
            .expect("stream renderer enqueued first text");
        assert_eq!(row.text, "hello");
        assert_eq!(row.turn_id.as_deref(), Some("turn-1"));
    }
}
