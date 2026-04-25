pub mod openai;

use anyhow::Result;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use serde::ser::SerializeStruct;
use serde_json::Value;
use tokio::sync::mpsc;

/// A message in the conversation history (OpenAI-compatible format)
#[derive(Debug, Clone)]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Option<Vec<ToolCall>>,
        /// Reasoning trace from thinking-mode models (DeepSeek V4 emits this
        /// as `reasoning_content`). When the conversation continues, the
        /// provider may require the prior assistant turn's reasoning to be
        /// echoed back — without it, the API rejects with 400. See YYC-43.
        reasoning_content: Option<String>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

/// Serialize Message to OpenAI chat format: {"role": "...", "content": "...", ...}
impl Serialize for Message {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Message::System { content } => {
                let mut s = serializer.serialize_struct("Message", 2)?;
                s.serialize_field("role", "system")?;
                s.serialize_field("content", content)?;
                s.end()
            }
            Message::User { content } => {
                let mut s = serializer.serialize_struct("Message", 2)?;
                s.serialize_field("role", "user")?;
                s.serialize_field("content", content)?;
                s.end()
            }
            Message::Assistant {
                content,
                tool_calls,
                reasoning_content,
            } => {
                let mut s = serializer.serialize_struct("Message", 4)?;
                s.serialize_field("role", "assistant")?;
                if let Some(c) = content {
                    s.serialize_field("content", c)?;
                } else {
                    s.serialize_field("content", "")?;
                }
                if let Some(tc) = tool_calls {
                    s.serialize_field("tool_calls", tc)?;
                }
                if let Some(rc) = reasoning_content {
                    s.serialize_field("reasoning_content", rc)?;
                }
                s.end()
            }
            Message::Tool { tool_call_id, content } => {
                let mut s = serializer.serialize_struct("Message", 3)?;
                s.serialize_field("role", "tool")?;
                s.serialize_field("tool_call_id", tool_call_id)?;
                s.serialize_field("content", content)?;
                s.end()
            }
        }
    }
}

/// Custom deserialization for Message (OpenAI format)
impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct MessageData {
            role: String,
            content: Option<String>,
            #[serde(default)]
            tool_call_id: Option<String>,
            #[serde(default)]
            tool_calls: Option<Vec<ToolCall>>,
            #[serde(default)]
            reasoning_content: Option<String>,
        }

        let data = MessageData::deserialize(deserializer)?;
        match data.role.as_str() {
            "system" => Ok(Message::System {
                content: data.content.unwrap_or_default(),
            }),
            "user" => Ok(Message::User {
                content: data.content.unwrap_or_default(),
            }),
            "assistant" => Ok(Message::Assistant {
                content: data.content,
                tool_calls: data.tool_calls,
                reasoning_content: data.reasoning_content,
            }),
            "tool" => Ok(Message::Tool {
                tool_call_id: data.tool_call_id.unwrap_or_default(),
                content: data.content.unwrap_or_default(),
            }),
            other => Err(de::Error::custom(format!("unknown role: {other}"))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

/// A tool definition sent to the LLM (OpenAI-compatible format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// Events emitted during streaming response
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A chunk of text content
    Text(String),
    /// A tool call was received (name + arguments-so-far)
    ToolCallStart { id: String, name: String },
    /// The stream is complete (with optional final ChatResponse)
    Done(ChatResponse),
    /// The stream hit an error
    Error(String),
}

/// A provider capable of chatting with an LLM
#[async_trait::async_trait]
pub trait LLMProvider: Send + Sync {
    /// Send a chat request and get a buffered response (for CLI mode).
    /// Race against `cancel.cancelled()`; on cancel, drop the in-flight HTTP
    /// request and return an `Err` (the agent loop translates this).
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<ChatResponse>;

    /// Send a chat request and stream response events through a channel (for TUI mode).
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        tx: mpsc::UnboundedSender<StreamEvent>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<()>;

    /// Get the model's context window size
    fn max_context(&self) -> usize;
}

/// Response from the LLM — either a text message or tool calls
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub usage: Option<Usage>,
    pub finish_reason: Option<String>,
    /// Reasoning trace from thinking-mode models. Carried through so the
    /// agent can attach it to the assistant message it appends to history.
    pub reasoning_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
}
