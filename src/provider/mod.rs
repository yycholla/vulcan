pub mod catalog;
pub mod openai;

#[cfg(test)]
pub mod mock;

use std::fmt;
use std::time::Duration;

use anyhow::Result;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;
use tokio::sync::mpsc;

/// Categorized provider failure. Replaces opaque `anyhow::bail!` strings with
/// a structured taxonomy so callers (retry logic, TUI banner) can branch on
/// the kind of failure, and so the user sees an actionable next-step hint
/// instead of raw provider JSON. See YYC-41.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// 401 / 403 — API key is missing, invalid, revoked, or lacks permission.
    Auth { message: String },
    /// 429 — provider is throttling. `retry_after` is the server's hint if it
    /// sent a `Retry-After` header.
    RateLimited { retry_after: Option<Duration> },
    /// 404 with a model-shaped error body — bad model slug.
    ModelNotFound { model: String, message: String },
    /// 400 / 422 — request shape is wrong (malformed messages, invalid params,
    /// missing reasoning_content, etc.).
    BadRequest { message: String },
    /// 5xx — provider is having a bad day.
    ServerError { status: u16, message: String },
    /// Connection/DNS/TLS/timeout — never reached the provider.
    Network(reqwest::Error),
    /// Status codes we don't classify (3xx redirects we can't follow,
    /// uncommon 4xx, etc.) and non-JSON 4xx bodies.
    Other { status: u16, body: String },
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderError::Auth { message } => write!(
                f,
                "Authentication failed: {message}. \
                 Check your API key in ~/.vulcan/config.toml or set the VULCAN_API_KEY env var."
            ),
            ProviderError::RateLimited {
                retry_after: Some(d),
            } => write!(
                f,
                "Rate limited by provider. Suggested retry after {}s.",
                d.as_secs().max(1)
            ),
            ProviderError::RateLimited { retry_after: None } => {
                write!(f, "Rate limited by provider. Retrying with backoff.")
            }
            ProviderError::ModelNotFound { model, message } => write!(
                f,
                "Model '{model}' not found ({message}). \
                 Check `[provider].model` in your config — model slugs are listed at the provider's catalog \
                 (e.g. https://openrouter.ai/models)."
            ),
            ProviderError::BadRequest { message } => write!(
                f,
                "Provider rejected the request: {message}. \
                 This usually means the message format is wrong (often a model-specific \
                 requirement around tool calls or reasoning passthrough)."
            ),
            ProviderError::ServerError { status, message } => write!(
                f,
                "Provider returned {status}: {message}. This is a transient server-side issue; \
                 retries should resolve it."
            ),
            ProviderError::Network(e) => write!(
                f,
                "Network error reaching provider: {e}. \
                 Check connectivity, base_url in config, and any proxy settings."
            ),
            ProviderError::Other { status, body } => {
                let trimmed = body.chars().take(300).collect::<String>();
                write!(f, "Provider returned {status}: {trimmed}")
            }
        }
    }
}

impl ProviderError {
    /// Should the agent retry this error within its budget?
    /// `Auth` / `BadRequest` / `ModelNotFound` / `Other` are non-retryable —
    /// retrying just spends budget on a guaranteed failure.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ProviderError::RateLimited { .. }
                | ProviderError::ServerError { .. }
                | ProviderError::Network(_)
        )
    }

    /// Classify a non-2xx HTTP response into a `ProviderError`. Tolerates
    /// non-JSON bodies and varying error shapes across OpenAI/OpenRouter/
    /// Anthropic (`error.message` + either `error.code` or `error.type`).
    pub fn from_response(status: reqwest::StatusCode, body: &str, model: &str) -> Self {
        let code = status.as_u16();
        let message = extract_error_message(body).unwrap_or_else(|| body.to_string());

        match code {
            401 | 403 => ProviderError::Auth { message },
            429 => ProviderError::RateLimited {
                retry_after: None, // header parsing is the caller's job
            },
            404 => {
                // Heuristic: if the body mentions the model slug, treat as ModelNotFound.
                if message.to_lowercase().contains("model")
                    || body.to_lowercase().contains(&model.to_lowercase())
                {
                    ProviderError::ModelNotFound {
                        model: model.to_string(),
                        message,
                    }
                } else {
                    ProviderError::Other {
                        status: code,
                        body: body.to_string(),
                    }
                }
            }
            400 | 422 => ProviderError::BadRequest { message },
            500..=599 => ProviderError::ServerError {
                status: code,
                message,
            },
            _ => ProviderError::Other {
                status: code,
                body: body.to_string(),
            },
        }
    }
}

impl From<reqwest::Error> for ProviderError {
    fn from(e: reqwest::Error) -> Self {
        ProviderError::Network(e)
    }
}

/// Best-effort extraction of `error.message` from common provider body shapes.
/// All of OpenAI / OpenRouter / Anthropic / DeepSeek wrap errors as
/// `{"error": {"message": "...", ...}}`; OpenRouter's nested `metadata.raw`
/// also follows that shape one level deeper.
fn extract_error_message(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    // Top-level error.message
    if let Some(m) = v
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
    {
        // OpenRouter-style: error.metadata.raw is a JSON string with an inner
        // error.message that's the actual provider message. Surface that if present.
        if let Some(raw) = v
            .get("error")
            .and_then(|e| e.get("metadata"))
            .and_then(|md| md.get("raw"))
            .and_then(|r| r.as_str())
        {
            if let Ok(inner) = serde_json::from_str::<serde_json::Value>(raw) {
                if let Some(inner_m) = inner
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                {
                    return Some(inner_m.to_string());
                }
            }
        }
        return Some(m.to_string());
    }
    // Some providers (Anthropic) put it at top level
    v.get("message")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string())
}

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
                let mut s = serializer.serialize_struct("Message", 5)?;
                s.serialize_field("role", "assistant")?;
                if let Some(c) = content {
                    s.serialize_field("content", c)?;
                } else {
                    s.serialize_field("content", "")?;
                }
                if let Some(tc) = tool_calls {
                    s.serialize_field("tool_calls", tc)?;
                }
                // Emit both field names. DeepSeek's native API uses
                // `reasoning_content`; OpenRouter's standardized name is
                // `reasoning`. Sending both means a tool-using turn against
                // either path won't fail with "must be passed back" (YYC-63).
                if let Some(rc) = reasoning_content {
                    s.serialize_field("reasoning_content", rc)?;
                    s.serialize_field("reasoning", rc)?;
                }
                s.end()
            }
            Message::Tool {
                tool_call_id,
                content,
            } => {
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
    /// A chunk of reasoning trace from a thinking-mode model (DeepSeek
    /// `reasoning_content`). Sent as it arrives so the UI can render the
    /// model "thinking" rather than blocking on the wait. See YYC-47.
    Reasoning(String),
    /// A tool call is starting to dispatch. Emitted by the agent loop just
    /// before `dispatch_tool` runs, so the TUI can render `🔧 name…` while
    /// the tool executes (otherwise the chat would stay stuck on "Thinking…"
    /// for the duration of the tool run). See YYC-57.
    ToolCallStart { id: String, name: String },
    /// A tool call has finished. `ok` reflects `ToolResult::is_error`
    /// (false on error/block/cancel). The TUI flips the corresponding
    /// in-flight marker from `🔧 name…` to `🔧 name ✓` / `✗`.
    ToolCallEnd { id: String, name: String, ok: bool },
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
