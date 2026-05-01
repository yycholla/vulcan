//! Telegram platform connector powered by teloxide-core.
//!
//! Two receive paths feed the same `InboundQueue`:
//!
//!   * Long-poll: `spawn_long_poll` runs `bot.get_updates()` with
//!     offset-based ack in a tokio task. Mirrors Discord's
//!     `spawn_gateway_client` — fire-and-forget JoinHandle.
//!   * Webhook: `verify_webhook` reads
//!     `X-Telegram-Bot-Api-Secret-Token` and constant-time compares
//!     via `subtle`. The /webhook/:platform route is shared with
//!     other platforms.
//!
//! Both paths normalize through `inbound_from_update_parts` (parts +
//! allow-list filter) so the filter logic lives in one place. Long-
//! poll feeds typed `Update`s straight into `inbound_from_update`;
//! the webhook path round-trips `serde_json::Value` -> typed `Update`
//! via `inbound_from_value` and then dispatches to the same typed
//! walker, so media extraction lives in one place. Live HTTP is
//! exercised by integration tests / manual smoke; the parsing
//! helpers are unit-tested.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use teloxide_core::{
    ApiError, Bot, RequestError,
    prelude::Requester,
    types::{ChatId, FileId, MessageId, ReplyParameters, Update, UpdateKind},
};
use tokio::task::JoinHandle;

use crate::gateway::queue::InboundQueue;
use crate::platform::{
    Attachment, AttachmentKind, InboundMessage, OutboundMessage, Platform, PlatformCapabilities,
    SentMessage,
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

    /// Pick the largest `PhotoSize` from the slice. Telegram returns
    /// multiple sizes per photo; the last is conventionally the
    /// largest, but `max_by_key` on the byte size is the robust pick.
    fn largest_photo_size(
        sizes: &[teloxide_core::types::PhotoSize],
    ) -> Option<&teloxide_core::types::PhotoSize> {
        sizes.iter().max_by_key(|p| p.file.size)
    }

    /// Extract attachments from a typed `Message`. At most one
    /// attachment per Message — Telegram doesn't multi-attach in one
    /// message the way Discord does. Order: photo → document → voice
    /// → video → audio → sticker.
    ///
    /// **Telegram-specific contract:** `Attachment.url` carries the
    /// opaque `file_id`, NOT a CDN URL. Telegram has no public CDN —
    /// `download_attachment` resolves the file_id via `bot.get_file`
    /// and streams the body. Discord stores real HTTPS URLs in the
    /// same field; same shape, platform-specific interpretation.
    fn attachments_from_message(msg: &teloxide_core::types::Message) -> Vec<Attachment> {
        use teloxide_core::types::{MediaKind, MessageKind};
        let common = match &msg.kind {
            MessageKind::Common(c) => c,
            _ => return Vec::new(),
        };
        let att = match &common.media_kind {
            MediaKind::Photo(p) => {
                let Some(largest) = Self::largest_photo_size(&p.photo) else {
                    return Vec::new();
                };
                Attachment {
                    url: Some(largest.file.id.0.clone()),
                    local_path: None,
                    mime: Some("image/jpeg".into()),
                    kind: AttachmentKind::Image,
                    original_name: None,
                }
            }
            MediaKind::Document(d) => Attachment {
                url: Some(d.document.file.id.0.clone()),
                local_path: None,
                mime: d.document.mime_type.as_ref().map(|m| m.to_string()),
                kind: AttachmentKind::Document,
                original_name: d.document.file_name.clone(),
            },
            MediaKind::Voice(v) => Attachment {
                url: Some(v.voice.file.id.0.clone()),
                local_path: None,
                mime: v.voice.mime_type.as_ref().map(|m| m.to_string()),
                kind: AttachmentKind::Voice,
                original_name: None,
            },
            MediaKind::Video(v) => Attachment {
                url: Some(v.video.file.id.0.clone()),
                local_path: None,
                mime: v.video.mime_type.as_ref().map(|m| m.to_string()),
                kind: AttachmentKind::Video,
                original_name: v.video.file_name.clone(),
            },
            MediaKind::Audio(a) => Attachment {
                url: Some(a.audio.file.id.0.clone()),
                local_path: None,
                mime: a.audio.mime_type.as_ref().map(|m| m.to_string()),
                kind: AttachmentKind::Audio,
                original_name: a.audio.file_name.clone(),
            },
            MediaKind::Sticker(s) => Attachment {
                url: Some(s.sticker.file.id.0.clone()),
                local_path: None,
                mime: None,
                kind: AttachmentKind::Sticker,
                original_name: None,
            },
            _ => return Vec::new(),
        };
        vec![att]
    }

    /// Build an InboundMessage from a Telegram Update's pieces. Centralized
    /// so the long-poll and webhook paths can share parsing + the
    /// allow-list filter. Returns None when the message should be
    /// dropped (allow-list miss, no content).
    pub(crate) fn inbound_from_update_parts(
        chat_id: i64,
        user_id: u64,
        message_id: i32,
        text: &str,
        reply_to: Option<i32>,
        attachments: Vec<Attachment>,
        allowed: &Option<HashSet<i64>>,
    ) -> Option<InboundMessage> {
        let trimmed = text.trim();
        // Allow empty text when there's at least one attachment — sending
        // an image-only message is normal Telegram usage. Mirrors the
        // PR-4 Discord behavior.
        if trimmed.is_empty() && attachments.is_empty() {
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
            scheduler_job_id: None,
            message_id: Some(message_id.to_string()),
            reply_to: reply_to.map(|r| r.to_string()),
            attachments,
        })
    }

    /// Walk a typed `Update` (from `Bot::get_updates`) into the parts
    /// `inbound_from_update_parts` consumes. Long-poll path uses this
    /// rather than a `to_value` round trip — cheaper and type-safe.
    /// Webhook path also routes through this (after typed
    /// deserialization in `inbound_from_value`) so media extraction
    /// lives in one place.
    pub(crate) fn inbound_from_update(
        update: &Update,
        allowed: &Option<HashSet<i64>>,
    ) -> Option<InboundMessage> {
        let m = match &update.kind {
            UpdateKind::Message(m) | UpdateKind::EditedMessage(m) => m,
            _ => return None,
        };
        let chat_id = m.chat.id.0;
        let user_id = m.from.as_ref()?.id.0;
        let message_id = m.id.0;
        // `Message::text()` returns `None` for media-only messages
        // (no caption AND no plain text). Default to "" so the
        // attachments-only path passes through `inbound_from_update_parts`.
        let text = m.text().unwrap_or("");
        let reply_to = m.reply_to_message().map(|r| r.id.0);
        let attachments = Self::attachments_from_message(m);
        Self::inbound_from_update_parts(
            chat_id,
            user_id,
            message_id,
            text,
            reply_to,
            attachments,
            allowed,
        )
    }

    /// Webhook path: bytes arrive as JSON. PR-3b routes through the
    /// typed `Update` deserializer so media extraction stays in one
    /// place (`inbound_from_update` / `attachments_from_message`).
    /// If the body isn't a parseable Update we drop it via `None`.
    ///
    /// Takes raw bytes — webhook handlers call this with the request
    /// body directly. teloxide-core 0.13's `UpdateKind` Deserialize
    /// visitor uses `next_key::<&str>()` and falls back to
    /// `Error(Value)` when driven from `serde_json::Value`'s owned-
    /// String keys, so we deserialize from `&[u8]` once and avoid the
    /// double round-trip through Value.
    pub(crate) fn inbound_from_webhook_body(
        body: &[u8],
        allowed: &Option<HashSet<i64>>,
    ) -> Option<InboundMessage> {
        let typed: Update = serde_json::from_slice(body).ok()?;
        Self::inbound_from_update(&typed, allowed)
    }

    /// Dispatch a single `OutboundAttachment` to the kind-specific
    /// send_* endpoint. Returns the platform's id for the sent
    /// message - the caller chains the first id into SentMessage so
    /// StreamRenderer's edit-in-place anchors target the right
    /// message.
    async fn send_attachment(
        &self,
        chat_id: ChatId,
        att: &crate::platform::OutboundAttachment,
        caption: Option<String>,
        reply: Option<ReplyParameters>,
    ) -> Result<String> {
        use teloxide_core::payloads::{
            SendAudioSetters, SendDocumentSetters, SendPhotoSetters, SendVideoSetters,
            SendVoiceSetters,
        };
        use teloxide_core::types::InputFile;
        let file = InputFile::file(&att.path);
        let sent = match att.kind {
            AttachmentKind::Image => {
                let mut req = self.bot.send_photo(chat_id, file);
                if let Some(c) = caption {
                    req = req.caption(c);
                }
                if let Some(rp) = reply {
                    req = req.reply_parameters(rp);
                }
                req.await.context("telegram send_photo failed")?
            }
            AttachmentKind::Voice => {
                let mut req = self.bot.send_voice(chat_id, file);
                if let Some(c) = caption {
                    req = req.caption(c);
                }
                if let Some(rp) = reply {
                    req = req.reply_parameters(rp);
                }
                req.await.context("telegram send_voice failed")?
            }
            AttachmentKind::Video => {
                let mut req = self.bot.send_video(chat_id, file);
                if let Some(c) = caption {
                    req = req.caption(c);
                }
                if let Some(rp) = reply {
                    req = req.reply_parameters(rp);
                }
                req.await.context("telegram send_video failed")?
            }
            AttachmentKind::Audio => {
                let mut req = self.bot.send_audio(chat_id, file);
                if let Some(c) = caption {
                    req = req.caption(c);
                }
                if let Some(rp) = reply {
                    req = req.reply_parameters(rp);
                }
                req.await.context("telegram send_audio failed")?
            }
            // Document / Sticker / Other -> send_document. Telegram has
            // no generic "any blob" endpoint; send_document accepts
            // arbitrary file types.
            AttachmentKind::Document | AttachmentKind::Sticker | AttachmentKind::Other => {
                let mut req = self.bot.send_document(chat_id, file);
                if let Some(c) = caption {
                    req = req.caption(c);
                }
                if let Some(rp) = reply {
                    req = req.reply_parameters(rp);
                }
                req.await.context("telegram send_document failed")?
            }
        };
        Ok(sent.id.0.to_string())
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

    /// Spawn the long-poll loop. Returns a JoinHandle the gateway can
    /// abort on shutdown. Errors inside the loop are logged + the loop
    /// continues — Telegram's getUpdates is idempotent (offset-based)
    /// so transient failures self-heal on the next call.
    pub fn spawn_long_poll(
        bot_token: String,
        allowed_chat_ids: Vec<i64>,
        poll_interval_secs: u32,
        inbound: Arc<InboundQueue>,
    ) -> Result<JoinHandle<()>> {
        if bot_token.trim().is_empty() {
            anyhow::bail!("gateway.telegram.bot_token is required when Telegram is enabled");
        }
        let allowed = allowed_set(allowed_chat_ids);
        let bot = Bot::new(bot_token);
        Ok(tokio::spawn(async move {
            run_long_poll(bot, allowed, poll_interval_secs, inbound).await;
        }))
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
        // Telegram rejects send_message with empty text, so harden the
        // contract upfront. StreamRenderer / OutboundDispatcher already
        // short-circuits empty buffers, but a future caller bypassing
        // them would otherwise see a generic "send_message failed"
        // anyhow chain instead of an actionable error.
        if msg.attachments.is_empty() && msg.text.trim().is_empty() {
            anyhow::bail!("telegram send: empty text and no attachments");
        }
        let chat_id = Self::chat_id_from_chat(&msg.chat_id)?;
        let reply_params = if let Some(reply_to) = &msg.reply_to {
            let reply_id = Self::message_id_from_str(reply_to)?;
            Some(ReplyParameters::new(reply_id))
        } else {
            None
        };

        // Telegram doesn't combine text + multiple media in a single
        // send the way Discord does. Strategy:
        //   * No attachments -> send_message (text only).
        //   * Attachments    -> send the first attachment with msg.text
        //                       as its caption (or the attachment's own
        //                       caption when msg.text is empty). Any
        //                       remaining attachments fire as separate
        //                       follow-up messages with their own
        //                       captions.
        // SentMessage carries the id of the FIRST sent message - that's
        // the anchor StreamRenderer/edit-in-place will target.
        if msg.attachments.is_empty() {
            let mut req = self.bot.send_message(chat_id, msg.text.clone());
            if let Some(rp) = reply_params {
                req = req.reply_parameters(rp);
            }
            let sent = req.await.context("telegram send_message failed")?;
            return Ok(SentMessage {
                message_id: sent.id.0.to_string(),
            });
        }

        let mut iter = msg.attachments.iter();
        let first = iter.next().expect("non-empty checked above");
        let first_caption = if msg.text.is_empty() {
            first.caption.clone()
        } else {
            Some(msg.text.clone())
        };
        let first_id = self
            .send_attachment(chat_id, first, first_caption, reply_params)
            .await?;
        for att in iter {
            let _ = self
                .send_attachment(chat_id, att, att.caption.clone(), None)
                .await
                .context("telegram follow-up attachment send failed")?;
        }
        Ok(SentMessage {
            message_id: first_id,
        })
    }

    async fn edit(&self, chat_id: &str, message_id: &str, text: &str) -> Result<()> {
        let chat = Self::chat_id_from_chat(chat_id)?;
        let id = Self::message_id_from_str(message_id)?;
        let result = self.bot.edit_message_text(chat, id, text.to_string()).await;
        // Telegram returns 400 "message is not modified" when the new text
        // matches the existing text byte-for-byte. That's an idempotency
        // win on our side, not a failure to surface. Match the typed
        // ApiError variant so unrelated errors (network, JSON, etc.) can
        // never trip this branch.
        if let Err(RequestError::Api(ApiError::MessageNotModified)) = &result {
            return Ok(());
        }
        result
            .map(|_| ())
            .context("telegram edit_message_text failed")
    }

    async fn recv(&self) -> Result<InboundMessage> {
        anyhow::bail!("telegram receives messages through the long-poll task or webhook")
    }

    async fn download_attachment(
        &self,
        att: &crate::platform::Attachment,
    ) -> Result<std::path::PathBuf> {
        use teloxide_core::net::Download;
        // Telegram-specific contract: Attachment.url stores the opaque
        // file_id (Telegram has no public CDN URL until bot.get_file
        // resolves it). Discord stores real HTTPS URLs in the same
        // field; same shape, platform-specific interpretation.
        let file_id = att.url.as_ref().ok_or_else(|| {
            anyhow::anyhow!("telegram attachment has no file_id (url field empty)")
        })?;
        let file = self
            .bot
            .get_file(FileId(file_id.clone()))
            .await
            .with_context(|| format!("telegram get_file({file_id}) failed"))?;

        let dir = crate::config::vulcan_home()
            .join("attachments")
            .join("telegram");
        tokio::fs::create_dir_all(&dir)
            .await
            .context("create attachments dir")?;
        // Filename strategy mirrors Discord's PR-4 hardening: take
        // Path::file_name() of the user-supplied original_name to
        // defang `../etc/passwd` style traversal. If the result is
        // None or degenerate (empty / "." / ".."), fall through to a
        // UUID so adversarial names can't collide on a literal landing
        // pad.
        let raw = att.original_name.as_deref();
        let stripped = raw
            .and_then(|n| std::path::Path::new(n).file_name())
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty() && *s != "." && *s != "..");
        let filename = match stripped {
            Some(n) => n.to_string(),
            None => format!("att-{}.bin", uuid::Uuid::new_v4()),
        };
        let path = dir.join(&filename);

        let mut f = tokio::fs::File::create(&path)
            .await
            .with_context(|| format!("create {}", path.display()))?;
        self.bot
            .download_file(&file.path, &mut f)
            .await
            .with_context(|| format!("telegram download_file({}) failed", file.path))?;

        // Best-effort sync; log on failure so flaky-disk diagnostics
        // are tractable but don't fail the agent.
        if let Err(e) = f.sync_all().await {
            tracing::warn!(
                target: "gateway::telegram",
                error = %e,
                path = %path.display(),
                "fsync failed during download_attachment; continuing",
            );
        }
        Ok(path)
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
        Self::inbound_from_webhook_body(body, &self.allowed_chat_ids)
            .ok_or_else(|| anyhow::anyhow!("Update did not parse to a usable InboundMessage"))
    }

    fn capabilities(&self) -> PlatformCapabilities {
        // PR-3b flips media on: send dispatches by AttachmentKind to
        // send_photo / send_voice / send_video / send_audio /
        // send_document; download_attachment resolves file_id via
        // bot.get_file and streams the body via bot.download_file.
        PlatformCapabilities {
            supports_edit: true,
            supports_media_send: true,
            supports_media_recv: true,
            supports_threads: true,
            // Telegram caps edits to ~1/sec/chat. The renderer's
            // throttle reads this to space out streaming chunks.
            edit_min_interval_ms: 1000,
        }
    }
}

async fn run_long_poll(
    bot: Bot,
    allowed: Option<HashSet<i64>>,
    poll_interval_secs: u32,
    inbound: Arc<InboundQueue>,
) {
    use teloxide_core::payloads::GetUpdatesSetters;
    let mut offset: i32 = 0;
    loop {
        let req = bot.get_updates().offset(offset).timeout(poll_interval_secs);
        match req.await {
            Ok(updates) => {
                for u in updates {
                    // UpdateId is u32; the GetUpdates `offset` parameter is
                    // i32. `try_from` converts cleanly when in range and
                    // saturates to i32::MAX otherwise, then +1. Telegram's
                    // update ids are monotonically increasing but bounded
                    // well below i32::MAX in practice.
                    offset = i32::try_from(u.id.0).unwrap_or(i32::MAX).saturating_add(1);
                    let Some(inb) = TelegramPlatform::inbound_from_update(&u, &allowed) else {
                        continue;
                    };
                    if let Err(e) = inbound.enqueue(inb).await {
                        tracing::error!(
                            target: "gateway::telegram",
                            error = %e,
                            "failed to enqueue telegram inbound",
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    target: "gateway::telegram",
                    error = %e,
                    "telegram getUpdates failed; backing off",
                );
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
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
        let inb = TelegramPlatform::inbound_from_update_parts(
            42,
            7,
            100,
            "hello",
            None,
            vec![],
            &allowed(&[]),
        )
        .expect("accepted");
        assert_eq!(inb.platform, "telegram");
        assert_eq!(inb.chat_id, "42");
        assert_eq!(inb.user_id, "7");
        assert_eq!(inb.message_id.as_deref(), Some("100"));
        assert!(inb.reply_to.is_none());
        assert_eq!(inb.text, "hello");
        assert!(inb.attachments.is_empty());
    }

    #[test]
    fn inbound_parts_handles_negative_chat_id_for_groups() {
        let inb = TelegramPlatform::inbound_from_update_parts(
            -1001234,
            7,
            100,
            "in a group",
            None,
            vec![],
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
            vec![],
            &allowed(&[]),
        )
        .expect("accepted");
        assert_eq!(inb.reply_to.as_deref(), Some("99"));
    }

    #[test]
    fn inbound_parts_drops_empty_text() {
        let inb = TelegramPlatform::inbound_from_update_parts(
            42,
            7,
            100,
            "   ",
            None,
            vec![],
            &allowed(&[]),
        );
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
            vec![],
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
            vec![],
            &allowed(&[42, 99]),
        );
        assert!(inb.is_some());
    }

    #[test]
    fn inbound_parts_accepts_empty_text_with_attachment() {
        let att = Attachment {
            url: Some("file_id_xyz".into()),
            local_path: None,
            mime: Some("image/jpeg".into()),
            kind: AttachmentKind::Image,
            original_name: None,
        };
        let inb = TelegramPlatform::inbound_from_update_parts(
            42,
            7,
            100,
            "",
            None,
            vec![att],
            &allowed(&[]),
        )
        .expect("photo-only message should pass");
        assert_eq!(inb.text, "");
        assert_eq!(inb.attachments.len(), 1);
        assert_eq!(inb.attachments[0].kind, AttachmentKind::Image);
        assert_eq!(inb.attachments[0].url.as_deref(), Some("file_id_xyz"));
    }

    #[test]
    fn inbound_parts_drops_when_text_empty_and_no_attachments() {
        let inb = TelegramPlatform::inbound_from_update_parts(
            42,
            7,
            100,
            "",
            None,
            vec![],
            &allowed(&[]),
        );
        assert!(inb.is_none());
    }

    #[test]
    fn new_rejects_empty_bot_token() {
        let err = TelegramPlatform::new("", vec![], None).expect_err("empty token must fail");
        assert!(err.to_string().contains("bot_token"));
    }

    #[test]
    fn capabilities_declare_edit_threads_and_media_true() {
        let p = TelegramPlatform::new("Bot 123:abc", vec![], None).expect("ctor");
        let caps = p.capabilities();
        assert!(caps.supports_edit);
        assert!(caps.supports_threads);
        assert!(caps.supports_media_send);
        assert!(caps.supports_media_recv);
        assert_eq!(caps.edit_min_interval_ms, 1000);
    }

    #[test]
    fn inbound_from_webhook_body_extracts_text_message() {
        // PR-3b: inbound_from_webhook_body deserializes raw bytes
        // straight into a typed `Update` — fixture must be a complete
        // Update shape (update_id, full from, chat.type, date).
        let body = br#"{
            "update_id": 1,
            "message": {
                "message_id": 100,
                "from": { "id": 7, "is_bot": false, "first_name": "tester" },
                "chat": { "id": 42, "first_name": "tester", "type": "private" },
                "date": 1700000000,
                "text": "hello bot"
            }
        }"#;
        let inb = TelegramPlatform::inbound_from_webhook_body(body, &allowed(&[])).expect("parse");
        assert_eq!(inb.text, "hello bot");
        assert_eq!(inb.chat_id, "42");
        assert_eq!(inb.message_id.as_deref(), Some("100"));
    }

    #[test]
    fn inbound_from_webhook_body_extracts_photo_attachment() {
        // PR-3b: webhook bodies carrying photo media route through the
        // typed Update walker — the file_id of the largest PhotoSize
        // lands in Attachment.url, mime is "image/jpeg" (Telegram
        // doesn't expose actual mime per PhotoSize), kind is Image.
        let body = br#"{
            "update_id": 2,
            "message": {
                "message_id": 200,
                "from": { "id": 7, "is_bot": false, "first_name": "tester" },
                "chat": { "id": 42, "first_name": "tester", "type": "private" },
                "date": 1700000000,
                "photo": [
                    { "file_id": "small", "file_unique_id": "u1", "width": 90,  "height": 90,  "file_size": 100 },
                    { "file_id": "large", "file_unique_id": "u2", "width": 800, "height": 800, "file_size": 9999 }
                ]
            }
        }"#;
        let inb = TelegramPlatform::inbound_from_webhook_body(body, &allowed(&[]))
            .expect("photo-only message should parse");
        assert_eq!(inb.text, "");
        assert_eq!(inb.attachments.len(), 1);
        assert_eq!(
            inb.attachments[0].url.as_deref(),
            Some("large"),
            "largest PhotoSize wins",
        );
        assert_eq!(
            inb.attachments[0].kind,
            crate::platform::AttachmentKind::Image
        );
        assert_eq!(inb.attachments[0].mime.as_deref(), Some("image/jpeg"));
    }

    /// Typed-Update sibling of `inbound_from_value_extracts_text_message`.
    /// Constructs a real `teloxide_core::types::Update` via JSON round
    /// trip (its derived Deserialize is the supported public API for
    /// fixture construction) and asserts the typed walker pulls the
    /// same parts the JSON walker does.
    #[test]
    fn inbound_from_update_extracts_text_message() {
        let json = r#"{
            "update_id": 1,
            "message": {
                "message_id": 100,
                "from": {
                    "id": 7,
                    "is_bot": false,
                    "first_name": "tester"
                },
                "chat": {
                    "id": 42,
                    "first_name": "tester",
                    "type": "private"
                },
                "date": 1700000000,
                "text": "hello bot"
            }
        }"#;
        let update: teloxide_core::types::Update =
            serde_json::from_str(json).expect("deserialize Update");
        let inb = TelegramPlatform::inbound_from_update(&update, &allowed(&[])).expect("parse");
        assert_eq!(inb.text, "hello bot");
        assert_eq!(inb.chat_id, "42");
        assert_eq!(inb.user_id, "7");
        assert_eq!(inb.message_id.as_deref(), Some("100"));
    }

    /// Typed-error path for "message is not modified": construct a
    /// `RequestError::Api(ApiError::MessageNotModified)` directly (the
    /// teloxide-core 0.13 variants we matched on at the call site) and
    /// confirm the discriminant pattern matches it. We can't easily
    /// drive `TelegramPlatform::edit` to return this without a live
    /// network round trip, but matching the pattern here is enough to
    /// catch a future teloxide-core rename.
    #[test]
    fn typed_message_not_modified_pattern_matches() {
        let err: Result<(), RequestError> = Err(RequestError::Api(ApiError::MessageNotModified));
        let swallowed = matches!(&err, Err(RequestError::Api(ApiError::MessageNotModified)));
        assert!(
            swallowed,
            "expected typed MessageNotModified pattern to match"
        );
    }

    fn webhook_body() -> Vec<u8> {
        // PR-3b: must be a complete Update shape so the typed
        // deserializer in `inbound_from_value` accepts it.
        serde_json::to_vec(&serde_json::json!({
            "update_id": 1,
            "message": {
                "message_id": 100,
                "from": { "id": 7, "is_bot": false, "first_name": "tester" },
                "chat": { "id": 42, "first_name": "tester", "type": "private" },
                "date": 1700000000,
                "text": "hi via webhook",
            },
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn verify_webhook_accepts_matching_secret() {
        let p = TelegramPlatform::new("123:abc", vec![], Some("s3cret".into())).expect("ctor");
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "x-telegram-bot-api-secret-token",
            http::HeaderValue::from_static("s3cret"),
        );
        let inb = p
            .verify_webhook(&headers, &webhook_body())
            .await
            .expect("accepted");
        assert_eq!(inb.text, "hi via webhook");
        assert_eq!(inb.chat_id, "42");
    }

    #[tokio::test]
    async fn verify_webhook_rejects_mismatched_secret() {
        let p = TelegramPlatform::new("123:abc", vec![], Some("s3cret".into())).expect("ctor");
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "x-telegram-bot-api-secret-token",
            http::HeaderValue::from_static("wrong"),
        );
        let err = p
            .verify_webhook(&headers, &webhook_body())
            .await
            .expect_err("must reject");
        assert!(
            err.to_string().contains("invalid webhook secret"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn verify_webhook_rejects_missing_header() {
        let p = TelegramPlatform::new("123:abc", vec![], Some("s3cret".into())).expect("ctor");
        let headers = http::HeaderMap::new();
        let err = p
            .verify_webhook(&headers, &webhook_body())
            .await
            .expect_err("must reject");
        assert!(
            err.to_string()
                .contains("missing X-Telegram-Bot-Api-Secret-Token"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn verify_webhook_rejects_when_not_configured() {
        let p = TelegramPlatform::new("123:abc", vec![], None).expect("ctor");
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "x-telegram-bot-api-secret-token",
            http::HeaderValue::from_static("anything"),
        );
        let err = p
            .verify_webhook(&headers, &webhook_body())
            .await
            .expect_err("must reject");
        assert!(
            err.to_string().contains("webhook not configured"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn send_rejects_empty_text_and_no_attachments() {
        // Defensive contract: Telegram rejects send_message with empty
        // text. Caller (StreamRenderer / OutboundDispatcher) already
        // short-circuits empty buffers, but the platform shouldn't
        // surface a generic "send_message failed" anyhow chain when a
        // future caller bypasses them.
        let p = TelegramPlatform::new("123:abc", vec![], None).expect("ctor");
        let msg = OutboundMessage {
            platform: "telegram".into(),
            chat_id: "42".into(),
            text: "   ".into(),
            attachments: vec![],
            reply_to: None,
            edit_target: None,
            turn_id: None,
        };
        let err = p.send(&msg).await.expect_err("empty send must bail");
        assert!(
            err.to_string().contains("empty text and no attachments"),
            "unexpected error: {err}"
        );
    }
}
