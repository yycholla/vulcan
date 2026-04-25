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

/// A platform that can send and receive messages
#[async_trait::async_trait]
pub trait Platform: Send + Sync {
    fn name(&self) -> &str;
    async fn start(&self) -> Result<()>;
    async fn send(&self, msg: &OutboundMessage) -> Result<()>;
    async fn recv(&self) -> Result<InboundMessage>;
}
