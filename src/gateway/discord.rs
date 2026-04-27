//! Discord platform connector powered by Serenity.

use std::sync::Arc;

use crate::gateway::queue::InboundQueue;
use crate::platform::{InboundMessage, OutboundMessage, Platform};
use anyhow::{Context, Result};
use serenity::client::{Client, Context as SerenityContext, EventHandler};
use serenity::http::Http;
use serenity::model::channel::Message;
use serenity::model::id::ChannelId;
use serenity::prelude::GatewayIntents;
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
pub struct DiscordPlatform {
    http: Arc<Http>,
}

impl DiscordPlatform {
    pub fn new(bot_token: impl Into<String>) -> Result<Self> {
        let bot_token = bot_token.into();
        if bot_token.trim().is_empty() {
            anyhow::bail!("gateway.discord.bot_token is required when Discord is enabled");
        }
        Ok(Self {
            http: Arc::new(Http::new(&bot_token)),
        })
    }

    fn inbound_from_message_parts(
        channel_id: u64,
        user_id: u64,
        message_id: u64,
        author_is_bot: bool,
        content: &str,
        allow_bots: bool,
        reply_to: Option<u64>,
        attachments: Vec<crate::platform::Attachment>,
    ) -> Option<InboundMessage> {
        let text = content.trim();
        // Allow empty text when there's at least one attachment — sending
        // an image-only message is normal Discord usage.
        if (text.is_empty() && attachments.is_empty()) || (author_is_bot && !allow_bots) {
            return None;
        }

        Some(InboundMessage {
            platform: "discord".into(),
            chat_id: channel_id.to_string(),
            user_id: user_id.to_string(),
            text: text.to_string(),
            message_id: Some(message_id.to_string()),
            reply_to: reply_to.map(|r| r.to_string()),
            attachments,
        })
    }

    fn map_attachment(att: &serenity::model::channel::Attachment) -> crate::platform::Attachment {
        use crate::platform::{Attachment, AttachmentKind};
        // Robust MIME-prefix classifier: handle parameters
        // ("image/png; charset=utf-8") and case ("IMAGE/PNG").
        let kind = att
            .content_type
            .as_deref()
            .map(|mime| {
                let primary = mime
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .split('/')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                match primary.as_str() {
                    "image" => AttachmentKind::Image,
                    "video" => AttachmentKind::Video,
                    "audio" => AttachmentKind::Audio,
                    _ => AttachmentKind::Document,
                }
            })
            .unwrap_or(AttachmentKind::Other);
        Attachment {
            url: Some(att.url.clone()),
            local_path: None,
            mime: att.content_type.clone(),
            kind,
            original_name: Some(att.filename.clone()),
        }
    }

    fn channel_id_from_chat(chat_id: &str) -> Result<u64> {
        chat_id
            .parse::<u64>()
            .with_context(|| format!("invalid Discord channel id '{chat_id}'"))
    }

    pub fn spawn_gateway_client(
        bot_token: String,
        allow_bots: bool,
        inbound: Arc<InboundQueue>,
    ) -> Result<JoinHandle<()>> {
        Self::validate_bot_token(&bot_token)?;
        Ok(tokio::spawn(async move {
            if let Err(e) = run_gateway_client(bot_token, allow_bots, inbound).await {
                tracing::error!(target: "gateway::discord", error = %e, "discord gateway client stopped");
            }
        }))
    }

    fn validate_bot_token(bot_token: &str) -> Result<()> {
        if bot_token.trim().is_empty() {
            anyhow::bail!("gateway.discord.bot_token is required when Discord is enabled");
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Platform for DiscordPlatform {
    fn name(&self) -> &str {
        "discord"
    }

    async fn start(&self) -> Result<()> {
        Ok(())
    }

    async fn send(&self, msg: &OutboundMessage) -> Result<crate::platform::SentMessage> {
        use serenity::builder::{CreateAttachment, CreateMessage};
        use serenity::model::channel::MessageReference;
        use serenity::model::id::MessageId;
        let channel_id = ChannelId::new(Self::channel_id_from_chat(&msg.chat_id)?);
        let mut create = CreateMessage::new().content(&msg.text);
        if let Some(reply_to) = &msg.reply_to {
            let parent_id = MessageId::new(
                reply_to
                    .parse::<u64>()
                    .with_context(|| format!("invalid Discord reply_to id '{reply_to}'"))?,
            );
            create = create.reference_message(MessageReference::from((channel_id, parent_id)));
        }
        for att in &msg.attachments {
            let attach = CreateAttachment::path(&att.path)
                .await
                .with_context(|| format!("discord attachment '{}'", att.path.display()))?;
            let attach = if let Some(caption) = &att.caption {
                attach.description(caption.clone())
            } else {
                attach
            };
            create = create.add_file(attach);
        }
        let sent = channel_id
            .send_message(&self.http, create)
            .await
            .context("discord send_message failed")?;
        Ok(crate::platform::SentMessage {
            message_id: sent.id.get().to_string(),
        })
    }

    async fn edit(&self, chat_id: &str, message_id: &str, text: &str) -> Result<()> {
        use serenity::builder::EditMessage;
        use serenity::model::id::MessageId;
        let channel_id = ChannelId::new(Self::channel_id_from_chat(chat_id)?);
        let message_id = MessageId::new(
            message_id
                .parse::<u64>()
                .with_context(|| format!("invalid Discord message_id '{message_id}'"))?,
        );
        channel_id
            .edit_message(&self.http, message_id, EditMessage::new().content(text))
            .await
            .context("discord edit_message failed")?;
        Ok(())
    }

    async fn recv(&self) -> Result<InboundMessage> {
        anyhow::bail!("discord receives messages through the Serenity gateway task")
    }

    async fn download_attachment(
        &self,
        att: &crate::platform::Attachment,
    ) -> Result<std::path::PathBuf> {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;
        let url = att
            .url
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("attachment has no URL"))?;
        let resp = reqwest::get(url)
            .await
            .with_context(|| format!("fetch {url}"))?
            .error_for_status()
            .with_context(|| format!("non-2xx fetching {url}"))?;

        let dir = crate::config::vulcan_home()
            .join("attachments")
            .join("discord");
        tokio::fs::create_dir_all(&dir)
            .await
            .context("create attachments dir")?;
        // Filename strategy: take Path::file_name() of the network-supplied
        // name to defang `../etc/passwd` style traversal. If the result is
        // None or degenerate (empty / "."  / ".."), fall through to a UUID
        // so adversarial names can't collide on a literal "attachment"
        // landing pad.
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
        // Stream the body chunk-by-chunk so a 500MB Nitro attachment
        // doesn't materialize fully in memory before the first byte
        // hits disk.
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| format!("read chunk from {url}"))?;
            f.write_all(&chunk)
                .await
                .with_context(|| format!("write {}", path.display()))?;
        }
        // Best-effort sync; log on failure so flaky-disk diagnostics
        // are tractable but don't fail the agent.
        if let Err(e) = f.sync_all().await {
            tracing::warn!(
                target: "gateway::discord",
                error = %e,
                path = %path.display(),
                "fsync failed during download_attachment; continuing",
            );
        }
        Ok(path)
    }

    fn capabilities(&self) -> crate::platform::PlatformCapabilities {
        // PR-4 flips edit + media + threads on. edit_min_interval_ms = 200ms
        // gives headroom under Discord's 5/5s edit floor.
        crate::platform::PlatformCapabilities {
            supports_edit: true,
            supports_media_send: true,
            supports_media_recv: true,
            supports_threads: true,
            edit_min_interval_ms: 200,
        }
    }
}

struct DiscordEventHandler {
    inbound: Arc<InboundQueue>,
    allow_bots: bool,
}

#[serenity::async_trait]
impl EventHandler for DiscordEventHandler {
    async fn message(&self, _ctx: SerenityContext, msg: Message) {
        let attachments = msg
            .attachments
            .iter()
            .map(DiscordPlatform::map_attachment)
            .collect();
        let reply_to = msg.referenced_message.as_ref().map(|m| m.id.get());
        let Some(inbound) = DiscordPlatform::inbound_from_message_parts(
            msg.channel_id.get(),
            msg.author.id.get(),
            msg.id.get(),
            msg.author.bot,
            &msg.content,
            self.allow_bots,
            reply_to,
            attachments,
        ) else {
            return;
        };

        if let Err(e) = self.inbound.enqueue(inbound).await {
            tracing::error!(target: "gateway::discord", error = %e, "failed to enqueue discord inbound message");
        }
    }
}

async fn run_gateway_client(
    bot_token: String,
    allow_bots: bool,
    inbound: Arc<InboundQueue>,
) -> Result<()> {
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;
    let handler = DiscordEventHandler {
        inbound,
        allow_bots,
    };
    let mut client = Client::builder(&bot_token, intents)
        .event_handler(handler)
        .await
        .context("failed to build Discord gateway client")?;
    client
        .start()
        .await
        .context("discord gateway client failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbound_from_message_parts_uses_channel_as_chat_id() {
        let inbound = DiscordPlatform::inbound_from_message_parts(
            42,
            7,
            100,
            false,
            "hello vulcan",
            false,
            None,
            vec![],
        )
        .expect("message should be accepted");

        assert_eq!(inbound.platform, "discord");
        assert_eq!(inbound.chat_id, "42");
        assert_eq!(inbound.user_id, "7");
        assert_eq!(inbound.text, "hello vulcan");
        assert_eq!(inbound.message_id.as_deref(), Some("100"));
    }

    #[test]
    fn inbound_from_message_parts_ignores_bots_by_default() {
        let inbound = DiscordPlatform::inbound_from_message_parts(
            42,
            7,
            100,
            true,
            "bot noise",
            false,
            None,
            vec![],
        );
        assert!(inbound.is_none());
    }

    #[test]
    fn inbound_from_message_parts_can_allow_bots() {
        let inbound = DiscordPlatform::inbound_from_message_parts(
            42,
            7,
            100,
            true,
            "bot relay",
            true,
            None,
            vec![],
        );
        assert!(inbound.is_some());
    }

    #[test]
    fn inbound_from_message_parts_ignores_empty_text() {
        let inbound = DiscordPlatform::inbound_from_message_parts(
            42,
            7,
            100,
            false,
            "   ",
            false,
            None,
            vec![],
        );
        assert!(inbound.is_none());
    }

    #[test]
    fn inbound_from_message_parts_carries_typed_attachments() {
        use crate::platform::{Attachment, AttachmentKind};
        let att = Attachment {
            url: Some("https://cdn.discord/foo.png".into()),
            local_path: None,
            mime: Some("image/png".into()),
            kind: AttachmentKind::Image,
            original_name: Some("foo.png".into()),
        };
        let inb = DiscordPlatform::inbound_from_message_parts(
            42,
            7,
            100,
            false,
            "look at this",
            false,
            None,
            vec![att],
        )
        .expect("accepted");
        assert_eq!(inb.attachments.len(), 1);
        assert_eq!(inb.attachments[0].kind, AttachmentKind::Image);
    }

    #[test]
    fn inbound_from_message_parts_accepts_empty_text_with_attachment() {
        use crate::platform::{Attachment, AttachmentKind};
        let att = Attachment {
            url: Some("https://cdn.discord/foo.png".into()),
            local_path: None,
            mime: Some("image/png".into()),
            kind: AttachmentKind::Image,
            original_name: Some("foo.png".into()),
        };
        let inb = DiscordPlatform::inbound_from_message_parts(
            42,
            7,
            100,
            false,
            "   ",
            false,
            None,
            vec![att],
        );
        assert!(inb.is_some(), "image-only message must not be dropped");
    }

    #[test]
    fn inbound_from_message_parts_propagates_reply_to() {
        let inb = DiscordPlatform::inbound_from_message_parts(
            42,
            7,
            100,
            false,
            "reply",
            false,
            Some(99),
            vec![],
        )
        .expect("accepted");
        assert_eq!(inb.reply_to.as_deref(), Some("99"));
    }

    #[test]
    fn map_attachment_classifies_kind_from_mime() {
        use serenity::model::channel::Attachment as SerenityAtt;
        // Serenity's Attachment has many fields with no public ctor;
        // construct via JSON deserialize. Required fields per the
        // Serenity 0.12 struct: id, filename, proxy_url, size, url
        // (description, height, width, content_type, duration_secs are
        // Option, and waveform/ephemeral default).
        // Tied to serenity 0.12 Attachment shape; if a bump fails this
        // test, see serenity::model::channel::Attachment fields.
        let json = serde_json::json!({
            "id": "1",
            "filename": "x.png",
            "size": 0,
            "url": "https://cdn.discord/x.png",
            "proxy_url": "https://cdn.discord/x.png",
            "content_type": "image/png",
        });
        let serenity_att: SerenityAtt = serde_json::from_value(json).expect("att deser");
        let mapped = DiscordPlatform::map_attachment(&serenity_att);
        assert_eq!(mapped.kind, crate::platform::AttachmentKind::Image);
        assert_eq!(mapped.mime.as_deref(), Some("image/png"));
        assert_eq!(mapped.original_name.as_deref(), Some("x.png"));
        assert_eq!(mapped.url.as_deref(), Some("https://cdn.discord/x.png"));
    }

    #[test]
    fn map_attachment_handles_mime_with_parameters_and_case() {
        use serenity::model::channel::Attachment as SerenityAtt;
        // Real Discord uploads occasionally carry parameters
        // ("image/png; charset=utf-8") or non-canonical case
        // ("IMAGE/PNG"). The classifier should still land Image —
        // a stricter prefix check would silently mis-bucket as
        // Document and lose downstream rendering hints.
        let cases = [
            (
                "image/png; charset=utf-8",
                crate::platform::AttachmentKind::Image,
            ),
            ("IMAGE/PNG", crate::platform::AttachmentKind::Image),
            ("  video/mp4 ", crate::platform::AttachmentKind::Video),
            ("audio/ogg", crate::platform::AttachmentKind::Audio),
            ("application/pdf", crate::platform::AttachmentKind::Document),
        ];
        for (mime, expected) in cases {
            let json = serde_json::json!({
                "id": "1",
                "filename": "x",
                "size": 0,
                "url": "https://cdn.discord/x",
                "proxy_url": "https://cdn.discord/x",
                "content_type": mime,
            });
            let att: SerenityAtt = serde_json::from_value(json).expect("att deser");
            assert_eq!(
                DiscordPlatform::map_attachment(&att).kind,
                expected,
                "mime '{mime}' should map to {expected:?}",
            );
        }
    }

    #[test]
    fn new_rejects_empty_bot_token() {
        let err = DiscordPlatform::new("").expect_err("empty token should fail");
        assert!(err.to_string().contains("bot_token"));
    }

    #[test]
    fn channel_id_from_chat_rejects_non_numeric_chat_id() {
        let err = DiscordPlatform::channel_id_from_chat("not-a-channel").expect_err("invalid id");
        assert!(err.to_string().contains("invalid Discord channel id"));
    }

    #[test]
    fn discord_capabilities_now_declare_edit_and_media_true() {
        // PR-4 flip: edit, media send/recv, threads all true.
        let p = DiscordPlatform::new("Bot fake.token.xyz").expect("ctor");
        let caps = p.capabilities();
        assert!(caps.supports_edit);
        assert!(caps.supports_media_send);
        assert!(caps.supports_media_recv);
        assert!(caps.supports_threads);
        assert_eq!(caps.edit_min_interval_ms, 200);
    }

    #[tokio::test]
    async fn message_id_from_str_parses_numeric_id() {
        // Indirect test: the parse path inside send returns a parse
        // error for non-numeric reply_to ids before any HTTP work.
        use crate::platform::OutboundMessage;
        let p = DiscordPlatform::new("Bot fake.token.xyz").expect("ctor");
        let bad = OutboundMessage {
            platform: "discord".into(),
            chat_id: "42".into(),
            text: "hi".into(),
            attachments: vec![],
            reply_to: Some("not-a-number".into()),
            edit_target: None,
            turn_id: None,
        };
        let err = p.send(&bad).await.expect_err("invalid id");
        assert!(err.to_string().contains("invalid Discord reply_to id"));
    }
}
