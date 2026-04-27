/// Platform connector trait — for future Telegram/Discord/CLI support
///
/// This module defines the abstraction for sending and receiving messages
/// across different platforms. Phase 1 uses the CLI platform (stdin/stdout/TUI).
/// Phase 2 adds Telegram (teloxide) and Discord (serenity).
use anyhow::Result;

/// A message from a user on any platform
#[derive(Debug, Clone)]
pub struct InboundMessage {
    pub platform: String,
    pub chat_id: String,
    pub user_id: String,
    pub text: String,
}

/// A message to deliver to a user
#[derive(Debug, Clone)]
pub struct OutboundMessage {
    pub platform: String,
    pub chat_id: String,
    pub text: String,
    pub attachments: Vec<String>,
}

/// Result of a successful `Platform::send`. Carries the platform's
/// message id so the caller can later target it for edit-in-place
/// streaming (YYC-18 PR-2 wires this into the StreamRenderer).
#[derive(Debug, Clone)]
pub struct SentMessage {
    pub message_id: String,
}

/// What a platform supports. Drives renderer behavior — the
/// StreamRenderer (PR-2) reads `supports_edit` to decide whether to
/// stream via edits or append fresh messages, and `edit_min_interval_ms`
/// to throttle edit calls under platform rate limits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlatformCapabilities {
    pub supports_edit: bool,
    pub supports_media_send: bool,
    pub supports_media_recv: bool,
    pub supports_threads: bool,
    /// Minimum interval between consecutive `edit` calls for the same
    /// message, in milliseconds. 0 means edits are not supported (or
    /// the platform doesn't impose a floor — but in practice the value
    /// is set when `supports_edit = true`).
    pub edit_min_interval_ms: u64,
}

/// Type of a media attachment. Drives the platform-side decoder
/// (Telegram has separate `send_photo` / `send_document` / `send_voice`
/// endpoints; Discord uploads any file the same way but the kind still
/// drives rendering hints).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    Document,
    Audio,
    Video,
    Voice,
    Sticker,
    #[default]
    Other,
}

/// An attachment received from a platform. `local_path` is populated
/// after `Platform::download_attachment` materializes the bytes.
/// Receivers store these on `InboundMessage.attachments` (PR-2).
#[derive(Debug, Clone)]
pub struct Attachment {
    pub url: Option<String>,
    pub local_path: Option<String>,
    pub mime: Option<String>,
    pub kind: AttachmentKind,
    pub original_name: Option<String>,
}

/// An attachment to send. `OutboundMessage` will gain a `Vec<Self>`
/// in PR-2 once StreamRenderer wires media into the outbound pipeline.
#[derive(Debug, Clone)]
pub struct OutboundAttachment {
    pub path: String,
    pub kind: AttachmentKind,
    pub caption: Option<String>,
}

/// A platform that can send and receive messages
#[async_trait::async_trait]
pub trait Platform: Send + Sync {
    fn name(&self) -> &str;
    async fn start(&self) -> Result<()>;
    async fn send(&self, msg: &OutboundMessage) -> Result<()>;
    async fn recv(&self) -> Result<InboundMessage>;

    /// Verify a platform-specific webhook request and return the parsed
    /// `InboundMessage`. The default impl errors so platforms that don't
    /// accept webhooks (loopback in production, future poll-only platforms)
    /// don't have to implement it. Webhook handlers (`POST /webhook/:platform`)
    /// pass the raw request headers + body so each platform can apply its own
    /// HMAC scheme + payload schema. Uses `http::HeaderMap` (not
    /// `axum::http::HeaderMap`) so this trait stays free of an axum dep —
    /// `http` is a small standalone crate axum re-exports anyway.
    async fn verify_webhook(
        &self,
        _headers: &http::HeaderMap,
        _body: &[u8],
    ) -> Result<InboundMessage> {
        anyhow::bail!("platform does not accept webhooks")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sent_message_carries_string_id() {
        let s = SentMessage {
            message_id: "abc123".into(),
        };
        assert_eq!(s.message_id, "abc123");
    }

    #[test]
    fn platform_capabilities_default_is_zero_features() {
        let caps = PlatformCapabilities::default();
        assert!(!caps.supports_edit);
        assert!(!caps.supports_media_send);
        assert!(!caps.supports_media_recv);
        assert!(!caps.supports_threads);
        assert_eq!(caps.edit_min_interval_ms, 0);
    }

    #[test]
    fn attachment_kind_default_is_other() {
        assert_eq!(AttachmentKind::default(), AttachmentKind::Other);
    }

    #[test]
    fn attachment_carries_optional_fields() {
        let a = Attachment {
            url: Some("https://x".into()),
            local_path: None,
            mime: Some("image/png".into()),
            kind: AttachmentKind::Image,
            original_name: Some("x.png".into()),
        };
        assert_eq!(a.kind, AttachmentKind::Image);
        assert!(a.local_path.is_none());
    }

    #[test]
    fn outbound_attachment_carries_path_and_kind() {
        let a = OutboundAttachment {
            path: "/tmp/x.png".into(),
            kind: AttachmentKind::Image,
            caption: None,
        };
        assert_eq!(a.path, "/tmp/x.png");
    }
}
