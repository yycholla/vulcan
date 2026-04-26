//! In-process `Platform` that records outbound messages for tests.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use tokio::sync::Mutex;

use crate::platform::{InboundMessage, OutboundMessage, Platform};

#[derive(Default)]
pub struct LoopbackPlatform {
    recorded: Arc<Mutex<Vec<OutboundMessage>>>,
    failures_remaining: Arc<AtomicUsize>,
}

impl LoopbackPlatform {
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure a `LoopbackPlatform` that errors on its first `n` send calls
    /// and succeeds afterwards. Used to drive the outbound dispatcher's
    /// backoff path in tests.
    pub fn failing_first(n: usize) -> Self {
        Self {
            recorded: Arc::default(),
            failures_remaining: Arc::new(AtomicUsize::new(n)),
        }
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
        // Atomic compare-and-decrement: if there are failures left, decrement
        // and error; else record and return Ok.
        loop {
            let cur = self.failures_remaining.load(Ordering::SeqCst);
            if cur == 0 {
                break;
            }
            if self
                .failures_remaining
                .compare_exchange(cur, cur - 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                anyhow::bail!("loopback simulated failure");
            }
        }
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

    #[tokio::test]
    async fn loopback_failing_first_errors_then_succeeds() {
        let lp = LoopbackPlatform::failing_first(2);
        let m = OutboundMessage {
            platform: "loopback".into(),
            chat_id: "c".into(),
            text: "x".into(),
            attachments: vec![],
        };
        assert!(lp.send(&m).await.is_err());
        assert!(lp.send(&m).await.is_err());
        assert!(lp.send(&m).await.is_ok());
        assert!(lp.send(&m).await.is_ok());
        assert_eq!(lp.recorded().await.len(), 2);
    }
}
