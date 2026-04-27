//! Routes outbound messages to the named `Platform` connector.
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow};

use crate::platform::{OutboundMessage, Platform};

#[derive(Default)]
pub struct PlatformRegistry {
    inner: HashMap<String, Arc<dyn Platform>>,
}

impl PlatformRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a platform under `name`. The name is the routing key used by
    /// `send` (matched against `OutboundMessage::platform`); it is independent
    /// of `Platform::name()`. Returns the previous entry if `name` was already
    /// registered, so callers can detect double-wiring.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        platform: Arc<dyn Platform>,
    ) -> Option<Arc<dyn Platform>> {
        self.inner.insert(name.into(), platform)
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Platform>> {
        self.inner.get(name).cloned()
    }

    pub async fn send(&self, msg: &OutboundMessage) -> Result<crate::platform::SentMessage> {
        let plat = self
            .get(&msg.platform)
            .ok_or_else(|| anyhow!("unknown platform: {}", msg.platform))?;
        plat.send(msg).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::loopback::LoopbackPlatform;
    use crate::platform::OutboundMessage;

    fn out_msg(platform: &str, chat: &str, text: &str) -> OutboundMessage {
        OutboundMessage {
            platform: platform.into(),
            chat_id: chat.into(),
            text: text.into(),
            attachments: vec![],
        }
    }

    #[tokio::test]
    async fn registry_send_routes_by_platform_name() {
        let lp = Arc::new(LoopbackPlatform::default());
        let mut reg = PlatformRegistry::new();
        reg.register("loopback", lp.clone());
        reg.send(&out_msg("loopback", "c", "hello")).await.unwrap();
        let recorded = lp.recorded().await;
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].text, "hello");
    }

    #[tokio::test]
    async fn registry_unknown_platform_errors() {
        let reg = PlatformRegistry::new();
        let err = reg
            .send(&out_msg("nope", "c", "x"))
            .await
            .expect_err("should error");
        assert!(err.to_string().contains("unknown platform"));
    }

    #[tokio::test]
    async fn registry_get_returns_none_for_missing() {
        let reg = PlatformRegistry::new();
        assert!(reg.get("nope").is_none());
    }

    #[tokio::test]
    async fn registry_send_returns_sent_message_id() {
        let lp = Arc::new(LoopbackPlatform::default());
        let mut reg = PlatformRegistry::new();
        reg.register("loopback", lp.clone());
        let sent = reg
            .send(&out_msg("loopback", "c", "hello"))
            .await
            .expect("send ok");
        assert!(!sent.message_id.is_empty(), "message id should be set");
        let recorded = lp.recorded().await;
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].text, "hello");
    }

    #[tokio::test]
    async fn register_returns_previous_entry() {
        let lp1 = Arc::new(LoopbackPlatform::default());
        let lp2 = Arc::new(LoopbackPlatform::default());
        let mut reg = PlatformRegistry::new();
        assert!(reg.register("loopback", lp1.clone()).is_none());
        let prev = reg.register("loopback", lp2);
        assert!(prev.is_some());
        assert!(Arc::ptr_eq(&prev.unwrap(), &(lp1 as Arc<dyn Platform>)));
    }
}
