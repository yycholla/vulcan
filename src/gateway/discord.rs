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

    /// YYC-19: allowlist gate. Returns true when the message
    /// should be accepted. Empty allowlists mean "open" so the
    /// default DiscordConfig behaves like the pre-allowlist code.
    /// DM messages (no `guild_id`) are always allowed — DMs are
    /// already gated by who the bot has DM'd; locking them out by
    /// guild id would lock the bot out of itself.
    pub(crate) fn passes_allowlist(
        guild_id: Option<u64>,
        channel_id: u64,
        allowed_guild_ids: &[u64],
        allowed_channel_ids: &[u64],
    ) -> bool {
        if let Some(gid) = guild_id
            && !allowed_guild_ids.is_empty()
            && !allowed_guild_ids.contains(&gid)
        {
            return false;
        }
        if !allowed_channel_ids.is_empty() && !allowed_channel_ids.contains(&channel_id) {
            return false;
        }
        true
    }

    /// YYC-19: mention gate. Returns true when the bot should
    /// respond. Configurable via `require_mention`. DMs (no
    /// guild_id) always pass because addressing the bot in a DM is
    /// already a mention. In guild channels with `require_mention =
    /// true`, the message must mention the bot's user id.
    pub(crate) fn passes_mention_filter(
        guild_id: Option<u64>,
        require_mention: bool,
        bot_user_id: Option<u64>,
        mentioned_user_ids: &[u64],
    ) -> bool {
        if !require_mention {
            return true;
        }
        if guild_id.is_none() {
            return true;
        }
        match bot_user_id {
            Some(bot_id) => mentioned_user_ids.iter().any(|id| *id == bot_id),
            // YYC-19: if we couldn't determine the bot's user id
            // at startup, fail open — better to over-respond than
            // silently lock the bot out of every channel.
            None => true,
        }
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

    /// YYC-19: Discord's hard character cap on a single message.
    /// 2000 is the standard tier limit; nitro raises it but we
    /// always target the conservative bound. Outbound text longer
    /// than this is split via `split_for_discord` and delivered as
    /// a sequence of follow-up messages on the same channel.
    pub(crate) const DISCORD_MAX_CHARS: usize = 2000;

    /// YYC-19: split `text` into Discord-sized chunks. Each chunk
    /// is at most `DISCORD_MAX_CHARS` characters (counted as
    /// codepoints, not bytes — multibyte UTF-8 stays whole).
    /// Splits prefer newline boundaries, then space boundaries,
    /// then a hard char-index cut.
    pub(crate) fn split_for_discord(text: &str) -> Vec<String> {
        if text.chars().count() <= Self::DISCORD_MAX_CHARS {
            return vec![text.to_string()];
        }
        let mut out: Vec<String> = Vec::new();
        let mut remaining = text;
        while !remaining.is_empty() {
            if remaining.chars().count() <= Self::DISCORD_MAX_CHARS {
                out.push(remaining.to_string());
                break;
            }
            // Byte index of the (DISCORD_MAX_CHARS)-th codepoint.
            let split_byte = remaining
                .char_indices()
                .nth(Self::DISCORD_MAX_CHARS)
                .map(|(idx, _)| idx)
                .unwrap_or(remaining.len());
            let head = &remaining[..split_byte];
            // Prefer to cut at a newline; fall back to last space;
            // last resort is the hard char-index split.
            let take_to = head
                .rfind('\n')
                .map(|i| i + 1)
                .or_else(|| head.rfind(' ').map(|i| i + 1))
                .unwrap_or(split_byte);
            out.push(remaining[..take_to].to_string());
            remaining = &remaining[take_to..];
        }
        out
    }

    fn channel_id_from_chat(chat_id: &str) -> Result<u64> {
        chat_id
            .parse::<u64>()
            .with_context(|| format!("invalid Discord channel id '{chat_id}'"))
    }

    pub fn spawn_gateway_client(
        bot_token: String,
        allow_bots: bool,
        allowed_guild_ids: Vec<u64>,
        allowed_channel_ids: Vec<u64>,
        require_mention: bool,
        inbound: Arc<InboundQueue>,
    ) -> Result<JoinHandle<()>> {
        Self::validate_bot_token(&bot_token)?;
        Ok(tokio::spawn(async move {
            if let Err(e) = run_gateway_client(
                bot_token,
                allow_bots,
                allowed_guild_ids,
                allowed_channel_ids,
                require_mention,
                inbound,
            )
            .await
            {
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
        // YYC-19: Discord caps each message at 2000 characters.
        // Long replies are split; the first chunk carries the reply
        // context and any attachments, follow-ups are plain text on
        // the same channel. The returned message_id refers to the
        // first chunk so subsequent edits target a stable anchor.
        let mut chunks = Self::split_for_discord(&msg.text).into_iter();
        let first = chunks.next().unwrap_or_default();

        let mut create = CreateMessage::new().content(first);
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
        let first_id = sent.id.get().to_string();

        for follow in chunks {
            let create = CreateMessage::new().content(follow);
            if let Err(e) = channel_id.send_message(&self.http, create).await {
                tracing::warn!(
                    target: "gateway::discord",
                    error = %e,
                    chat_id = %msg.chat_id,
                    "follow-up Discord chunk failed; remaining text dropped",
                );
                break;
            }
        }

        Ok(crate::platform::SentMessage {
            message_id: first_id,
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
            Some(n) => format!("{}-{}", uuid::Uuid::new_v4(), n),
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
    /// YYC-19: guild allowlist. Empty = open (default).
    allowed_guild_ids: Vec<u64>,
    /// YYC-19: channel allowlist. Empty = open (default).
    allowed_channel_ids: Vec<u64>,
    /// YYC-19: when true, only respond in guild channels when
    /// mentioned. DMs always pass.
    require_mention: bool,
    /// YYC-19: bot's own user id, fetched once at gateway startup
    /// via `Http::get_current_user`. `None` falls open on the
    /// mention filter so a transient API hiccup doesn't lock the
    /// bot out of every channel.
    bot_user_id: Option<u64>,
}

#[serenity::async_trait]
impl EventHandler for DiscordEventHandler {
    async fn message(&self, _ctx: SerenityContext, msg: Message) {
        // YYC-19: drop messages from unallowed guilds/channels
        // before any further work. Cheaper than queueing and
        // dropping later, and keeps the inbound queue clean of
        // poisoned rows from public-server crosschat.
        if !DiscordPlatform::passes_allowlist(
            msg.guild_id.map(|g| g.get()),
            msg.channel_id.get(),
            &self.allowed_guild_ids,
            &self.allowed_channel_ids,
        ) {
            return;
        }
        // YYC-19: in guild channels, optionally require an explicit
        // bot mention before responding. Stops the bot from chiming
        // in on every public-channel message.
        let mentioned: Vec<u64> = msg.mentions.iter().map(|u| u.id.get()).collect();
        if !DiscordPlatform::passes_mention_filter(
            msg.guild_id.map(|g| g.get()),
            self.require_mention,
            self.bot_user_id,
            &mentioned,
        ) {
            return;
        }
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
    allowed_guild_ids: Vec<u64>,
    allowed_channel_ids: Vec<u64>,
    require_mention: bool,
    inbound: Arc<InboundQueue>,
) -> Result<()> {
    // YYC-19: fetch the bot's own user id once at startup so the
    // mention filter can compare against it without a per-message
    // round trip. `None` on failure leaves the filter open.
    let bot_user_id = match Http::new(&bot_token).get_current_user().await {
        Ok(user) => Some(user.id.get()),
        Err(e) => {
            tracing::warn!(
                target: "gateway::discord",
                error = %e,
                "could not fetch Discord bot user id; require_mention will fail open",
            );
            None
        }
    };
    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;
    let handler = DiscordEventHandler {
        inbound,
        allow_bots,
        allowed_guild_ids,
        allowed_channel_ids,
        require_mention,
        bot_user_id,
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

    // YYC-19: empty allowlists keep the door open (default).
    #[test]
    fn passes_allowlist_open_by_default() {
        assert!(DiscordPlatform::passes_allowlist(Some(1), 2, &[], &[]));
        assert!(DiscordPlatform::passes_allowlist(None, 2, &[], &[]));
    }

    // YYC-19: a guild allowlist drops messages from non-listed guilds
    // but still lets DMs (no guild_id) through.
    #[test]
    fn passes_allowlist_filters_by_guild() {
        assert!(!DiscordPlatform::passes_allowlist(
            Some(99),
            42,
            &[1, 2, 3],
            &[],
        ));
        assert!(DiscordPlatform::passes_allowlist(
            Some(2),
            42,
            &[1, 2, 3],
            &[]
        ));
        assert!(
            DiscordPlatform::passes_allowlist(None, 42, &[1, 2, 3], &[]),
            "DMs (no guild) must always pass guild allowlist",
        );
    }

    // YYC-19: a channel allowlist drops messages from non-listed
    // channels regardless of guild.
    #[test]
    fn passes_allowlist_filters_by_channel() {
        assert!(!DiscordPlatform::passes_allowlist(
            Some(1),
            42,
            &[],
            &[100, 200],
        ));
        assert!(DiscordPlatform::passes_allowlist(
            Some(1),
            100,
            &[],
            &[100, 200],
        ));
    }

    // YYC-19: require_mention=false is a no-op (default).
    #[test]
    fn passes_mention_filter_open_by_default() {
        assert!(DiscordPlatform::passes_mention_filter(
            Some(1),
            false,
            Some(99),
            &[]
        ));
        assert!(DiscordPlatform::passes_mention_filter(
            None,
            false,
            Some(99),
            &[]
        ));
    }

    // YYC-19: in DMs, require_mention is bypassed because the DM
    // itself is the addressing.
    #[test]
    fn passes_mention_filter_dms_always_pass() {
        assert!(DiscordPlatform::passes_mention_filter(
            None,
            true,
            Some(99),
            &[]
        ));
    }

    // YYC-19: in guild channels with require_mention, only messages
    // that mention the bot pass.
    #[test]
    fn passes_mention_filter_guild_requires_mention() {
        assert!(!DiscordPlatform::passes_mention_filter(
            Some(1),
            true,
            Some(99),
            &[42, 7], // bot id 99 not mentioned
        ));
        assert!(DiscordPlatform::passes_mention_filter(
            Some(1),
            true,
            Some(99),
            &[7, 99, 42],
        ));
    }

    // YYC-19: missing bot_user_id (startup fetch failed) falls open
    // so a transient API hiccup doesn't lock the bot out.
    #[test]
    fn passes_mention_filter_falls_open_when_bot_id_unknown() {
        assert!(DiscordPlatform::passes_mention_filter(
            Some(1),
            true,
            None,
            &[],
        ));
    }

    // YYC-19: text under the cap returns one chunk verbatim.
    #[test]
    fn split_for_discord_short_text_is_single_chunk() {
        let chunks = DiscordPlatform::split_for_discord("hello world");
        assert_eq!(chunks, vec!["hello world".to_string()]);
    }

    // YYC-19: text exactly at the cap is one chunk (boundary).
    #[test]
    fn split_for_discord_exactly_at_cap_is_one_chunk() {
        let s = "x".repeat(DiscordPlatform::DISCORD_MAX_CHARS);
        let chunks = DiscordPlatform::split_for_discord(&s);
        assert_eq!(chunks.len(), 1);
        assert_eq!(
            chunks[0].chars().count(),
            DiscordPlatform::DISCORD_MAX_CHARS
        );
    }

    // YYC-19: text over the cap splits into multiple chunks; each
    // chunk respects the char cap.
    #[test]
    fn split_for_discord_long_text_chunks_respect_cap() {
        let s = "abc\n".repeat(800); // 3200 chars
        let chunks = DiscordPlatform::split_for_discord(&s);
        assert!(chunks.len() >= 2, "got {} chunks", chunks.len());
        for chunk in &chunks {
            assert!(
                chunk.chars().count() <= DiscordPlatform::DISCORD_MAX_CHARS,
                "chunk over cap: {}",
                chunk.chars().count(),
            );
        }
        // Round-trip: rejoining the chunks recovers the original.
        let joined: String = chunks.into_iter().collect();
        assert_eq!(joined, s);
    }

    // YYC-19: chunk boundaries prefer newlines over hard cuts.
    #[test]
    fn split_for_discord_prefers_newline_boundary() {
        let mut s = "a".repeat(1990);
        s.push('\n');
        s.push_str(&"b".repeat(20));
        let chunks = DiscordPlatform::split_for_discord(&s);
        assert!(chunks.len() >= 2);
        assert!(
            chunks[0].ends_with('\n'),
            "first chunk should end at the newline, got: {:?}",
            &chunks[0][chunks[0].len().saturating_sub(5)..],
        );
    }

    // YYC-19: multibyte UTF-8 is counted as codepoints, not bytes,
    // so a chunk doesn't slice through a 4-byte emoji.
    #[test]
    fn split_for_discord_counts_codepoints_not_bytes() {
        let s = "🌊".repeat(1500); // 1500 codepoints, 6000 bytes
        let chunks = DiscordPlatform::split_for_discord(&s);
        assert_eq!(chunks.len(), 1, "1500 codepoints fits under the 2000 cap");
    }

    // YYC-19: when both filters set, both must match.
    #[test]
    fn passes_allowlist_requires_both_when_both_set() {
        assert!(DiscordPlatform::passes_allowlist(
            Some(1),
            100,
            &[1],
            &[100]
        ));
        assert!(!DiscordPlatform::passes_allowlist(
            Some(2),
            100,
            &[1],
            &[100]
        ));
        assert!(!DiscordPlatform::passes_allowlist(
            Some(1),
            999,
            &[1],
            &[100]
        ));
    }

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
