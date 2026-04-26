//! In-process `Platform` that records outbound messages for tests.
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;

use crate::platform::{InboundMessage, OutboundMessage, Platform};

#[derive(Default)]
pub struct LoopbackPlatform {
    recorded: Arc<Mutex<Vec<OutboundMessage>>>,
}

impl LoopbackPlatform {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn recorded(&self) -> Vec<OutboundMessage> {
        self.recorded.lock().await.clone()
    }
}

#[async_trait::async_trait]
impl Platform for LoopbackPlatform {
    fn name(&self) -> &str {
        "loopback"
    }

    async fn start(&self) -> Result<()> {
        Ok(())
    }

    async fn send(&self, msg: &OutboundMessage) -> Result<()> {
        self.recorded.lock().await.push(msg.clone());
        Ok(())
    }

    async fn recv(&self) -> Result<InboundMessage> {
        // Loopback has no inbound side. Return an error so a tokio::select!
        // arm completes and the caller can match-and-skip; a never-resolving
        // future would silently starve any consumer that polls multiple
        // platforms.
        anyhow::bail!("loopback has no inbound channel")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn loopback_send_records_messages_in_order() {
        let lp = LoopbackPlatform::new();
        let m1 = OutboundMessage {
            platform: "loopback".into(),
            chat_id: "c".into(),
            text: "one".into(),
            attachments: vec![],
        };
        let m2 = OutboundMessage {
            text: "two".into(),
            ..m1.clone()
        };
        lp.send(&m1).await.unwrap();
        lp.send(&m2).await.unwrap();
        let recorded = lp.recorded().await;
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].text, "one");
        assert_eq!(recorded[1].text, "two");
    }
}
