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
        author_is_bot: bool,
        content: &str,
        allow_bots: bool,
    ) -> Option<InboundMessage> {
        let text = content.trim();
        if text.is_empty() || (author_is_bot && !allow_bots) {
            return None;
        }

        Some(InboundMessage {
            platform: "discord".into(),
            chat_id: channel_id.to_string(),
            user_id: user_id.to_string(),
            text: text.to_string(),
        })
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
        let channel_id = ChannelId::new(Self::channel_id_from_chat(&msg.chat_id)?);
        let sent = channel_id
            .say(&self.http, &msg.text)
            .await
            .context("discord send failed")?;
        Ok(crate::platform::SentMessage {
            message_id: sent.id.get().to_string(),
        })
    }

    async fn recv(&self) -> Result<InboundMessage> {
        anyhow::bail!("discord receives messages through the Serenity gateway task")
    }

    fn capabilities(&self) -> crate::platform::PlatformCapabilities {
        // PR-1 declares only what's wired today (text-only send). PR-4
        // flips edit + media + threads to true alongside their wire impls.
        crate::platform::PlatformCapabilities::default()
    }
}

struct DiscordEventHandler {
    inbound: Arc<InboundQueue>,
    allow_bots: bool,
}

#[serenity::async_trait]
impl EventHandler for DiscordEventHandler {
    async fn message(&self, _ctx: SerenityContext, msg: Message) {
        let Some(inbound) = DiscordPlatform::inbound_from_message_parts(
            msg.channel_id.get(),
            msg.author.id.get(),
            msg.author.bot,
            &msg.content,
            self.allow_bots,
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
        let inbound =
            DiscordPlatform::inbound_from_message_parts(42, 7, false, "hello vulcan", false)
                .expect("message should be accepted");

        assert_eq!(inbound.platform, "discord");
        assert_eq!(inbound.chat_id, "42");
        assert_eq!(inbound.user_id, "7");
        assert_eq!(inbound.text, "hello vulcan");
    }

    #[test]
    fn inbound_from_message_parts_ignores_bots_by_default() {
        let inbound = DiscordPlatform::inbound_from_message_parts(42, 7, true, "bot noise", false);
        assert!(inbound.is_none());
    }

    #[test]
    fn inbound_from_message_parts_can_allow_bots() {
        let inbound = DiscordPlatform::inbound_from_message_parts(42, 7, true, "bot relay", true);
        assert!(inbound.is_some());
    }

    #[test]
    fn inbound_from_message_parts_ignores_empty_text() {
        let inbound = DiscordPlatform::inbound_from_message_parts(42, 7, false, "   ", false);
        assert!(inbound.is_none());
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
    fn discord_capabilities_declare_no_features_for_now() {
        // PR-1 declares Discord's current shipping surface (no edit, no
        // typed media). PR-4 flips these to true alongside the matching
        // wire impls.
        let p = DiscordPlatform::new("Bot abc.def.ghi").expect("ctor");
        let caps = p.capabilities();
        assert!(!caps.supports_edit);
        assert!(!caps.supports_media_send);
        assert!(!caps.supports_media_recv);
        assert!(!caps.supports_threads);
    }
}
