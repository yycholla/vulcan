//! Gateway inbound Turn delivery.
//!
//! Owns the inbound-row to outbound-result workflow: slash command
//! shortcut, daemon Session routing, prompt streaming, atomic success
//! completion, and failure marking.

use crate::daemon::protocol::StreamFrame;
use crate::gateway::commands::{CommandDispatcher, DispatchCtx};
use crate::gateway::daemon_client::GatewayDaemonClient;
use crate::gateway::lane::LaneKey;
use crate::gateway::lane_router::DaemonLaneRouter;
use crate::gateway::queue::{InboundQueue, InboundRow};
use crate::platform::OutboundMessage;

pub(crate) struct TurnDelivery<'a> {
    lane_router: &'a DaemonLaneRouter,
    daemon_client: &'a GatewayDaemonClient,
    inbound_queue: &'a InboundQueue,
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
        commands: &'a CommandDispatcher,
    ) -> Self {
        Self {
            lane_router,
            daemon_client,
            inbound_queue,
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
            Ok(reply_text) => {
                self.complete(row, reply_text).await?;
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
    /// Drains text chunks into the returned reply string. The final
    /// response's `text` field (if any) takes precedence over the
    /// accumulated chunks so we don't double-emit.
    async fn run_prompt_via_daemon(&self, lane: &LaneKey, input: &str) -> anyhow::Result<String> {
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

        let mut reply = String::new();
        while let Some(frame) = stream.frames.recv().await {
            if let Some(chunk) = extract_text_chunk(&frame) {
                reply.push_str(&chunk);
            }
        }

        let final_response = stream
            .done
            .await
            .map_err(|_| anyhow::anyhow!("daemon dropped completion sender"))?
            .map_err(|e| anyhow::anyhow!("stream completion: {e}"))?;

        if let Some(err) = final_response.error {
            anyhow::bail!("daemon prompt.stream error [{}]: {}", err.code, err.message);
        }

        if let Some(result) = final_response.result {
            if let Some(final_text) = result.get("text").and_then(|v| v.as_str()) {
                return Ok(final_text.to_string());
            }
        }
        Ok(reply)
    }
}

/// Pull a `text` chunk out of a `StreamFrame` if the frame is on the
/// `text` channel. Returns `None` for any other channel.
fn extract_text_chunk(frame: &StreamFrame) -> Option<String> {
    if frame.stream != "text" {
        return None;
    }
    frame
        .data
        .get("chunk")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}
