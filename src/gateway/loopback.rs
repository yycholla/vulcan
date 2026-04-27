//! In-process `Platform` that records outbound messages for tests.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::platform::{InboundMessage, OutboundMessage, Platform};

type HmacSha256 = Hmac<Sha256>;

#[derive(Default)]
pub struct LoopbackPlatform {
    recorded: Arc<Mutex<Vec<OutboundMessage>>>,
    failures_remaining: Arc<AtomicUsize>,
    /// `None` means webhooks are disabled — `verify_webhook` will reject every
    /// request. Tests that exercise the webhook path use
    /// `with_webhook_secret`.
    webhook_secret: Arc<Option<String>>,
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
            webhook_secret: Arc::new(None),
        }
    }

    /// Configure a loopback that accepts webhook requests signed with `secret`
    /// using HMAC-SHA256 over the raw body, hex-encoded in the
    /// `X-Loopback-Signature` header.
    pub fn with_webhook_secret(secret: impl Into<String>) -> Self {
        Self {
            recorded: Arc::default(),
            failures_remaining: Arc::default(),
            webhook_secret: Arc::new(Some(secret.into())),
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

    async fn send(&self, msg: &OutboundMessage) -> Result<crate::platform::SentMessage> {
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
        Ok(crate::platform::SentMessage {
            message_id: Uuid::new_v4().to_string(),
        })
    }

    fn capabilities(&self) -> crate::platform::PlatformCapabilities {
        crate::platform::PlatformCapabilities::default()
    }

    async fn recv(&self) -> Result<InboundMessage> {
        // Loopback has no inbound side. Return an error so a tokio::select!
        // arm completes and the caller can match-and-skip; a never-resolving
        // future would silently starve any consumer that polls multiple
        // platforms.
        anyhow::bail!("loopback has no inbound channel")
    }

    async fn verify_webhook(
        &self,
        headers: &http::HeaderMap,
        body: &[u8],
    ) -> Result<InboundMessage> {
        let Some(secret) = self.webhook_secret.as_ref().as_deref() else {
            anyhow::bail!("loopback webhook not configured (no secret)");
        };
        let provided = headers
            .get("x-loopback-signature")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("missing X-Loopback-Signature header"))?;

        let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
            .map_err(|e| anyhow::anyhow!("hmac key error: {e}"))?;
        mac.update(body);
        let expected_bytes = mac.finalize().into_bytes();
        // Manual lower-case hex avoids an extra `hex` dep — the encoding fits
        // in one expression and matches what `hex::encode` would emit.
        let expected_hex: String = expected_bytes.iter().map(|b| format!("{b:02x}")).collect();

        // Constant-time compare so signature verification doesn't leak the
        // matching prefix length via byte-by-byte early-out.
        use subtle::ConstantTimeEq;
        if provided
            .as_bytes()
            .ct_eq(expected_hex.as_bytes())
            .unwrap_u8()
            == 0
        {
            anyhow::bail!("invalid signature");
        }

        // Body shape mirrors /v1/inbound: {chat_id, user_id, text}. The
        // platform field is implicit ("loopback") — webhook callers don't
        // need to repeat it, the route already carries that context.
        let parsed: serde_json::Value =
            serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("invalid JSON body: {e}"))?;
        Ok(InboundMessage {
            platform: "loopback".into(),
            chat_id: parsed["chat_id"].as_str().unwrap_or_default().to_string(),
            user_id: parsed["user_id"].as_str().unwrap_or_default().to_string(),
            text: parsed["text"].as_str().unwrap_or_default().to_string(),
            message_id: None,
            reply_to: None,
            attachments: vec![],
        })
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
            reply_to: None,
            edit_target: None,
            turn_id: None,
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
            reply_to: None,
            edit_target: None,
            turn_id: None,
        };
        assert!(lp.send(&m).await.is_err());
        assert!(lp.send(&m).await.is_err());
        assert!(lp.send(&m).await.is_ok());
        assert!(lp.send(&m).await.is_ok());
        assert_eq!(lp.recorded().await.len(), 2);
    }

    #[tokio::test]
    async fn loopback_send_returns_unique_message_ids() {
        let lp = LoopbackPlatform::new();
        let m = OutboundMessage {
            platform: "loopback".into(),
            chat_id: "c".into(),
            text: "x".into(),
            attachments: vec![],
            reply_to: None,
            edit_target: None,
            turn_id: None,
        };
        let s1 = lp.send(&m).await.unwrap();
        let s2 = lp.send(&m).await.unwrap();
        assert_ne!(s1.message_id, s2.message_id);
        assert!(!s1.message_id.is_empty());
    }

    #[test]
    fn loopback_capabilities_declare_no_features_for_now() {
        let lp = LoopbackPlatform::new();
        let caps = lp.capabilities();
        assert!(!caps.supports_edit);
        assert!(!caps.supports_media_send);
    }
}
