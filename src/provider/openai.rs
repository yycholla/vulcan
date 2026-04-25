use crate::provider::{ChatResponse, LLMProvider, Message, StreamEvent, ToolCall, ToolDefinition, Usage};
use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::mpsc;

/// OpenAI-compatible provider (works with OpenRouter, Anthropic, Ollama, etc.)
pub struct OpenAIProvider {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    max_context: usize,
}

impl OpenAIProvider {
    pub fn new(
        base_url: &str,
        api_key: &str,
        model: &str,
        max_context: usize,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            max_context,
        })
    }

    fn build_request(&self, messages: &[Message], tools: &[ToolDefinition]) -> Value {
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            "max_tokens": 8096,
        });

        if !tools.is_empty() {
            body["tools"] = serde_json::to_value(tools).unwrap_or(json!([]));
        }

        body
    }

    /// Make the HTTP request and return the response
    async fn do_request(&self, messages: &[Message], tools: &[ToolDefinition]) -> Result<reqwest::Response> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = self.build_request(messages, tools);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("LLM API request failed")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("LLM API returned {status}: {text}");
        }

        Ok(response)
    }

    /// Parse a single SSE data line and accumulate state
    fn parse_line(
        line: &str,
        content: &mut String,
        tool_calls: &mut Vec<ToolCall>,
        usage: &mut Option<Usage>,
        finish_reason: &mut Option<String>,
    ) {
        let line = line.trim();
        if !line.starts_with("data: ") {
            return;
        }
        let data = line.strip_prefix("data: ").unwrap_or("");

        if data == "[DONE]" {
            return;
        }

        if let Ok(chunk) = serde_json::from_str::<Value>(data) {
            if let Some(choices) = chunk["choices"].as_array() {
                for choice in choices {
                    if let Some(delta) = choice["delta"].as_object() {
                        if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
                            content.push_str(text);
                        }
                        if let Some(tcs) = delta.get("tool_calls").and_then(|c| c.as_array()) {
                            for tc in tcs {
                                if let Some(idx) = tc.get("index").and_then(|i| i.as_u64()) {
                                    let idx = idx as usize;
                                    while tool_calls.len() <= idx {
                                        tool_calls.push(ToolCall {
                                            id: String::new(),
                                            call_type: "function".into(),
                                            function: crate::provider::ToolCallFunction {
                                                name: String::new(),
                                                arguments: String::new(),
                                            },
                                        });
                                    }
                                    if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                                        if !id.is_empty() {
                                            tool_calls[idx].id.push_str(id);
                                        }
                                    }
                                    if let Some(func) = tc.get("function") {
                                        if let Some(name) =
                                            func.get("name").and_then(|n| n.as_str())
                                        {
                                            tool_calls[idx].function.name.push_str(name);
                                        }
                                        if let Some(args) =
                                            func.get("arguments").and_then(|a| a.as_str())
                                        {
                                            tool_calls[idx].function.arguments.push_str(args);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let Some(reason) = choice["finish_reason"].as_str() {
                        if !reason.is_empty() && reason != "null" {
                            *finish_reason = Some(reason.to_string());
                        }
                    }
                }
            }
            if let Some(u) = chunk.get("usage") {
                if let (Some(prompt), Some(completion)) = (
                    u.get("prompt_tokens").and_then(|v| v.as_u64()),
                    u.get("completion_tokens").and_then(|v| v.as_u64()),
                ) {
                    *usage = Some(Usage {
                        prompt_tokens: prompt as usize,
                        completion_tokens: completion as usize,
                        total_tokens: (prompt + completion) as usize,
                    });
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl LLMProvider for OpenAIProvider {
    /// Buffered chat — collects full response, returns it all at once
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<ChatResponse> {
        let response = self.do_request(messages, tools).await?;
        let bytes = response.bytes().await.context("Failed to read response body")?;
        let text = String::from_utf8_lossy(&bytes);

        let mut content = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut usage: Option<Usage> = None;
        let mut finish_reason: Option<String> = None;

        for line in text.lines() {
            Self::parse_line(line, &mut content, &mut tool_calls, &mut usage, &mut finish_reason);
        }

        Ok(ChatResponse {
            content: Some(content).filter(|c| !c.is_empty()),
            tool_calls: Some(tool_calls).filter(|c| !c.is_empty()),
            usage,
            finish_reason,
        })
    }

    /// Streaming chat — sends text tokens through the channel as they arrive
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        tx: mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<()> {
        let response = self.do_request(messages, tools).await?;

        let mut content = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut usage: Option<Usage> = None;
        let mut finish_reason: Option<String> = None;

        // Read the HTTP response as a byte stream — chunks arrive as the LLM generates
        let mut stream = response.bytes_stream();
        let mut buf = String::new();

        use futures_util::StreamExt;
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.context("Failed to read stream chunk")?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            buf.push_str(&chunk_str);

            // Process complete lines from the buffer
            while let Some(newline) = buf.find('\n') {
                let line = buf[..newline].to_string();
                buf = buf[newline + 1..].to_string();

                let prev_len = content.len();
                Self::parse_line(&line, &mut content, &mut tool_calls, &mut usage, &mut finish_reason);

                // If content grew, send the delta through the channel
                if content.len() > prev_len {
                    let delta = &content[prev_len..];
                    let _ = tx.send(StreamEvent::Text(delta.to_string()));
                }
            }
        }

        // Build final ChatResponse and signal done
        let response = ChatResponse {
            content: Some(content).filter(|c| !c.is_empty()),
            tool_calls: Some(tool_calls).filter(|c| !c.is_empty()),
            usage,
            finish_reason,
        };

        let _ = tx.send(StreamEvent::Done(response));
        Ok(())
    }

    fn max_context(&self) -> usize {
        self.max_context
    }
}
