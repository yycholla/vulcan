//! Telegram platform connector powered by teloxide-core.
//!
//! `TelegramPlatform` implements `Platform` against teloxide-core's
//! HTTP client + types. Webhook receive: `verify_webhook` reads
//! `X-Telegram-Bot-Api-Secret-Token` and constant-time compares via
//! `subtle`. The /webhook/:platform route is shared with other
//! platforms.
//!
//! Inbound parsing flows through `inbound_from_value` (serde_json
//! Update shape parser) -> `inbound_from_update_parts` (parts +
//! allow-list filter) so the shape and filter logic lives in one
//! place. The long-poll receive task lives in a separate file-local
//! impl block and feeds the same helpers.

use std::collections::HashSet;

use anyhow::{Context, Result};
use teloxide_core::{
    Bot,
    prelude::Requester,
    types::{ChatId, MessageId, ReplyParameters},
};

use crate::platform::{
    InboundMessage, OutboundMessage, Platform, PlatformCapabilities, SentMessage,
};

#[derive(Debug, Clone)]
pub struct TelegramPlatform {
    bot: Bot,
    allowed_chat_ids: Option<HashSet<i64>>,
    webhook_secret: Option<String>,
}

impl TelegramPlatform {
    pub fn new(
        bot_token: impl Into<String>,
        allowed_chat_ids: Vec<i64>,
        webhook_secret: Option<String>,
    ) -> Result<Self> {
        let bot_token = bot_token.into();
        if bot_token.trim().is_empty() {
            anyhow::bail!("gateway.telegram.bot_token is required when Telegram is enabled");
        }
        let allowed = allowed_set(allowed_chat_ids);
        Ok(Self {
            bot: Bot::new(bot_token),
            allowed_chat_ids: allowed,
            webhook_secret,
        })
    }

    /// Build an InboundMessage from a Telegram Update's pieces. Centralized
    /// so the long-poll and webhook paths can share parsing + the
    /// allow-list filter. Returns None when the message should be
    /// dropped (allow-list miss, empty text).
    pub(crate) fn inbound_from_update_parts(
        chat_id: i64,
        user_id: u64,
        message_id: i32,
        text: &str,
        reply_to: Option<i32>,
        allowed: &Option<HashSet<i64>>,
    ) -> Option<InboundMessage> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        if let Some(set) = allowed
            && !set.contains(&chat_id)
        {
            return None;
        }
        Some(InboundMessage {
            platform: "telegram".into(),
            chat_id: chat_id.to_string(),
            user_id: user_id.to_string(),
            text: trimmed.to_string(),
            message_id: Some(message_id.to_string()),
            reply_to: reply_to.map(|r| r.to_string()),
            attachments: Vec::new(),
        })
    }

    /// Pull (chat_id, user_id, message_id, text, reply_to) out of a
    /// `serde_json::Value`-shaped Update and forward to
    /// `inbound_from_update_parts`. Used by both webhook and long-poll
    /// paths so JSON-shape parsing lives in one place.
    pub(crate) fn inbound_from_value(
        update: &serde_json::Value,
        allowed: &Option<HashSet<i64>>,
    ) -> Option<InboundMessage> {
        let msg = update
            .get("message")
            .or_else(|| update.get("edited_message"))?;
        let chat_id = msg.get("chat")?.get("id")?.as_i64()?;
        let user_id = msg.get("from")?.get("id")?.as_u64()?;
        let message_id = msg.get("message_id")?.as_i64()? as i32;
        let text = msg.get("text")?.as_str()?;
        let reply_to = msg
            .get("reply_to_message")
            .and_then(|r| r.get("message_id"))
            .and_then(|v| v.as_i64())
            .map(|n| n as i32);
        Self::inbound_from_update_parts(chat_id, user_id, message_id, text, reply_to, allowed)
    }

    fn chat_id_from_chat(chat: &str) -> Result<ChatId> {
        chat.parse::<i64>()
            .map(ChatId)
            .with_context(|| format!("invalid Telegram chat_id '{chat}'"))
    }

    fn message_id_from_str(id: &str) -> Result<MessageId> {
        id.parse::<i32>()
            .map(MessageId)
            .with_context(|| format!("invalid Telegram message_id '{id}'"))
    }
}

fn allowed_set(ids: Vec<i64>) -> Option<HashSet<i64>> {
    if ids.is_empty() {
        None
    } else {
        Some(ids.into_iter().collect())
    }
}

#[async_trait::async_trait]
impl Platform for TelegramPlatform {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn start(&self) -> Result<()> {
        Ok(())
    }

    async fn send(&self, msg: &OutboundMessage) -> Result<SentMessage> {
        use teloxide_core::payloads::SendMessageSetters;
        let chat_id = Self::chat_id_from_chat(&msg.chat_id)?;
        let mut req = self.bot.send_message(chat_id, msg.text.clone());
        if let Some(reply_to) = &msg.reply_to {
            let reply_id = Self::message_id_from_str(reply_to)?;
            req = req.reply_parameters(ReplyParameters::new(reply_id));
        }
        let sent = req.await.context("telegram send_message failed")?;
        Ok(SentMessage {
            message_id: sent.id.0.to_string(),
        })
    }

    async fn edit(&self, chat_id: &str, message_id: &str, text: &str) -> Result<()> {
        let chat = Self::chat_id_from_chat(chat_id)?;
        let id = Self::message_id_from_str(message_id)?;
        let result = self.bot.edit_message_text(chat, id, text.to_string()).await;
        if let Err(err) = &result {
            // Telegram returns 400 "message is not modified" when the
            // new text matches the existing text byte-for-byte. That's
            // an idempotency win on our side, not a failure to surface.
            let s = err.to_string();
            if s.contains("message is not modified") {
                return Ok(());
            }
        }
        result
            .map(|_| ())
            .context("telegram edit_message_text failed")
    }

    async fn recv(&self) -> Result<InboundMessage> {
        anyhow::bail!("telegram receives messages through the long-poll task or webhook")
    }

    async fn verify_webhook(
        &self,
        headers: &http::HeaderMap,
        body: &[u8],
    ) -> Result<InboundMessage> {
        let Some(expected) = &self.webhook_secret else {
            anyhow::bail!("telegram webhook not configured (no secret)");
        };
        let provided = headers
            .get("x-telegram-bot-api-secret-token")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| anyhow::anyhow!("missing X-Telegram-Bot-Api-Secret-Token header"))?;
        use subtle::ConstantTimeEq;
        if provided.as_bytes().ct_eq(expected.as_bytes()).unwrap_u8() == 0 {
            anyhow::bail!("invalid webhook secret");
        }
        let update: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| anyhow::anyhow!("invalid Update JSON: {e}"))?;
        Self::inbound_from_value(&update, &self.allowed_chat_ids)
            .ok_or_else(|| anyhow::anyhow!("Update did not parse to a usable InboundMessage"))
    }

    fn capabilities(&self) -> PlatformCapabilities {
        PlatformCapabilities {
            supports_edit: true,
            // PR-3 is text-only; the supports_media_* flags + send_photo /
            // send_document / download_attachment wiring land in PR-3b.
            supports_media_send: false,
            supports_media_recv: false,
            supports_threads: true,
            // Telegram caps edits to ~1/sec/chat. The renderer's
            // throttle reads this to space out streaming chunks.
            edit_min_interval_ms: 1000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(ids: &[i64]) -> Option<HashSet<i64>> {
        if ids.is_empty() {
            None
        } else {
            Some(ids.iter().copied().collect())
        }
    }

    #[test]
    fn inbound_parts_uses_chat_as_chat_id_and_carries_message_id() {
        let inb =
            TelegramPlatform::inbound_from_update_parts(42, 7, 100, "hello", None, &allowed(&[]))
                .expect("accepted");
        assert_eq!(inb.platform, "telegram");
        assert_eq!(inb.chat_id, "42");
        assert_eq!(inb.user_id, "7");
        assert_eq!(inb.message_id.as_deref(), Some("100"));
        assert!(inb.reply_to.is_none());
        assert_eq!(inb.text, "hello");
    }

    #[test]
    fn inbound_parts_handles_negative_chat_id_for_groups() {
        let inb = TelegramPlatform::inbound_from_update_parts(
            -1001234,
            7,
            100,
            "in a group",
            None,
            &allowed(&[]),
        )
        .expect("accepted");
        assert_eq!(inb.chat_id, "-1001234");
    }

    #[test]
    fn inbound_parts_carries_reply_to() {
        let inb = TelegramPlatform::inbound_from_update_parts(
            42,
            7,
            100,
            "thread reply",
            Some(99),
            &allowed(&[]),
        )
        .expect("accepted");
        assert_eq!(inb.reply_to.as_deref(), Some("99"));
    }

    #[test]
    fn inbound_parts_drops_empty_text() {
        let inb =
            TelegramPlatform::inbound_from_update_parts(42, 7, 100, "   ", None, &allowed(&[]));
        assert!(inb.is_none());
    }

    #[test]
    fn inbound_parts_drops_chat_not_in_allow_list() {
        let inb = TelegramPlatform::inbound_from_update_parts(
            42,
            7,
            100,
            "hi",
            None,
            &allowed(&[99, 100]),
        );
        assert!(inb.is_none());
    }

    #[test]
    fn inbound_parts_passes_chat_in_allow_list() {
        let inb = TelegramPlatform::inbound_from_update_parts(
            42,
            7,
            100,
            "hi",
            None,
            &allowed(&[42, 99]),
        );
        assert!(inb.is_some());
    }

    #[test]
    fn new_rejects_empty_bot_token() {
        let err = TelegramPlatform::new("", vec![], None).expect_err("empty token must fail");
        assert!(err.to_string().contains("bot_token"));
    }

    #[test]
    fn capabilities_declare_edit_and_threads_true_media_false() {
        let p = TelegramPlatform::new("Bot 123:abc", vec![], None).expect("ctor");
        let caps = p.capabilities();
        assert!(caps.supports_edit);
        assert!(caps.supports_threads);
        assert!(!caps.supports_media_send);
        assert!(!caps.supports_media_recv);
        assert_eq!(caps.edit_min_interval_ms, 1000);
    }

    #[test]
    fn inbound_from_value_extracts_text_message() {
        let update = serde_json::json!({
            "update_id": 1,
            "message": {
                "message_id": 100,
                "from": { "id": 7 },
                "chat": { "id": 42 },
                "text": "hello bot",
            },
        });
        let inb = TelegramPlatform::inbound_from_value(&update, &allowed(&[])).expect("parse");
        assert_eq!(inb.text, "hello bot");
        assert_eq!(inb.chat_id, "42");
        assert_eq!(inb.message_id.as_deref(), Some("100"));
    }
}
