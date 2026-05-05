use crate::config::ProviderDebugMode;
use crate::provider::{
    ChatResponse, LLMProvider, Message, ProviderError, StreamEvent, ToolCall, ToolCallFunction,
    ToolDefinition, Usage,
};
use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub struct ResponsesProvider {
    client: Client,
    base_url: String,
    token: String,
    model: String,
    max_context: usize,
    max_retries: u32,
    max_output_tokens: Option<usize>,
    debug_mode: ProviderDebugMode,
}

impl ResponsesProvider {
    pub fn new(
        base_url: &str,
        token: &str,
        model: &str,
        max_context: usize,
        max_retries: u32,
        max_output_tokens: Option<usize>,
        debug_mode: ProviderDebugMode,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            client,
            base_url: crate::provider::normalize_base_url(base_url),
            token: token.to_string(),
            model: model.to_string(),
            max_context,
            max_retries,
            max_output_tokens,
            debug_mode,
        })
    }

    fn build_request(&self, messages: &[Message], tools: &[ToolDefinition]) -> Value {
        let mut instructions = Vec::new();
        let mut input = Vec::new();

        for message in messages {
            match message {
                Message::System { content } => instructions.push(content.clone()),
                Message::User { content } => {
                    input.push(json!({
                        "role": "user",
                        "content": [{"type": "input_text", "text": content}],
                    }));
                }
                Message::Assistant {
                    content,
                    tool_calls,
                    ..
                } => {
                    if let Some(content) = content.as_ref().filter(|content| !content.is_empty()) {
                        input.push(json!({
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": content}],
                        }));
                    }
                    if let Some(tool_calls) = tool_calls {
                        for call in tool_calls {
                            input.push(json!({
                                "type": "function_call",
                                "call_id": call.id,
                                "name": call.function.name,
                                "arguments": call.function.arguments,
                            }));
                        }
                    }
                }
                Message::Tool {
                    tool_call_id,
                    content,
                } => {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": tool_call_id,
                        "output": content,
                    }));
                }
            }
        }

        let tools = tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.function.name,
                    "description": tool.function.description,
                    "parameters": tool.function.parameters,
                    "strict": false,
                })
            })
            .collect::<Vec<_>>();

        let mut body = json!({
            "model": self.model,
            "input": input,
            "stream": true,
            "store": false,
        });
        if !instructions.is_empty() {
            body["instructions"] = Value::String(instructions.join("\n\n"));
        }
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
            body["tool_choice"] = Value::String("auto".to_string());
            body["parallel_tool_calls"] = Value::Bool(true);
        }
        if let Some(max_output_tokens) = self.max_output_tokens {
            body["max_output_tokens"] = json!(max_output_tokens);
        }
        body
    }

    async fn do_request(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        cancel: &CancellationToken,
    ) -> std::result::Result<reqwest::Response, ProviderError> {
        let url = format!("{}/responses", self.base_url);
        let body = self.build_request(messages, tools);
        if self.debug_mode.logs_wire() {
            tracing::info!(
                provider = "openai-responses",
                model = %self.model,
                url = %url,
                request_body = %crate::provider::redact::redact_value(&body),
                "provider wire request"
            );
        }

        let mut last_err = None;
        for attempt in 0..=self.max_retries {
            if cancel.is_cancelled() {
                return Err(ProviderError::Other {
                    status: 0,
                    body: "Cancelled".into(),
                });
            }
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(250 * 2_u64.pow(attempt.min(5)))).await;
            }

            let send_result = self
                .client
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.token))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await;

            let err = match send_result {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    ProviderError::from_response(status, &body, &self.model)
                }
                Err(e) => ProviderError::Network(e),
            };

            if err.is_retryable() && attempt < self.max_retries {
                last_err = Some(err);
                continue;
            }
            return Err(err);
        }

        Err(last_err.unwrap_or(ProviderError::Other {
            status: 0,
            body: "retry budget exhausted".into(),
        }))
    }
}

#[async_trait::async_trait]
impl LLMProvider for ResponsesProvider {
    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        cancel: CancellationToken,
    ) -> Result<ChatResponse> {
        let (tx, mut rx) = mpsc::channel(crate::provider::STREAM_CHANNEL_CAPACITY);
        self.chat_stream(messages, tools, tx, cancel).await?;

        while let Some(event) = rx.recv().await {
            if let StreamEvent::Done(response) = event {
                return Ok(response);
            }
        }
        anyhow::bail!("Responses stream ended without final response")
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        tx: mpsc::Sender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Result<()> {
        let response = self.do_request(messages, tools, &cancel).await?;
        let mut stream = response.bytes_stream();
        let mut buf = String::new();
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = Vec::new();
        let mut finish_reason = None;
        let mut usage = None;

        use futures_util::StreamExt;
        while let Some(chunk) = tokio::select! {
            biased;
            _ = cancel.cancelled() => anyhow::bail!("Cancelled"),
            next = stream.next() => next,
        } {
            let chunk = chunk.context("Failed to read stream chunk")?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            drain_sse_buffer(
                &mut buf,
                &mut content,
                &mut reasoning,
                &mut tool_calls,
                &mut finish_reason,
                &mut usage,
                Some(&tx),
            )
            .await;
        }
        drain_sse_buffer(
            &mut buf,
            &mut content,
            &mut reasoning,
            &mut tool_calls,
            &mut finish_reason,
            &mut usage,
            Some(&tx),
        )
        .await;

        let response = ChatResponse {
            content: Some(content).filter(|content| !content.is_empty()),
            tool_calls: Some(tool_calls).filter(|calls| !calls.is_empty()),
            usage,
            finish_reason,
            reasoning_content: Some(reasoning).filter(|reasoning| !reasoning.is_empty()),
        };
        let _ = tx.send(StreamEvent::Done(response)).await;
        Ok(())
    }

    fn max_context(&self) -> usize {
        self.max_context
    }
}

async fn drain_sse_buffer(
    buf: &mut String,
    content: &mut String,
    reasoning: &mut String,
    tool_calls: &mut Vec<ToolCall>,
    finish_reason: &mut Option<String>,
    usage: &mut Option<Usage>,
    tx: Option<&mpsc::Sender<StreamEvent>>,
) {
    while let Some(newline) = buf.find('\n') {
        let line = buf[..newline].trim_end_matches('\r').to_string();
        *buf = buf[newline + 1..].to_string();
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        if data == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        apply_responses_event(
            &value,
            content,
            reasoning,
            tool_calls,
            finish_reason,
            usage,
            tx,
        )
        .await;
    }
}

async fn apply_responses_event(
    value: &Value,
    content: &mut String,
    reasoning: &mut String,
    tool_calls: &mut Vec<ToolCall>,
    finish_reason: &mut Option<String>,
    usage: &mut Option<Usage>,
    tx: Option<&mpsc::Sender<StreamEvent>>,
) {
    match value.get("type").and_then(Value::as_str) {
        Some("response.output_text.delta") => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                content.push_str(delta);
                if let Some(tx) = tx {
                    let _ = tx.send(StreamEvent::Text(delta.to_string())).await;
                }
            }
        }
        Some("response.reasoning_summary_text.delta" | "response.reasoning_text.delta") => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                reasoning.push_str(delta);
                if let Some(tx) = tx {
                    let _ = tx.send(StreamEvent::Reasoning(delta.to_string())).await;
                }
            }
        }
        Some("response.output_item.done") => {
            if let Some(item) = value.get("item")
                && item.get("type").and_then(Value::as_str) == Some("function_call")
            {
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("id").and_then(Value::as_str))
                    .unwrap_or("call");
                let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                tool_calls.push(ToolCall {
                    id: call_id.to_string(),
                    call_type: "function".to_string(),
                    function: ToolCallFunction {
                        name: name.to_string(),
                        arguments: arguments.to_string(),
                    },
                });
            }
        }
        Some("response.completed") => {
            *finish_reason = Some("stop".to_string());
            if let Some(total) = value
                .get("response")
                .and_then(|response| response.get("usage"))
                .and_then(|usage| usage.get("total_tokens"))
                .and_then(Value::as_u64)
            {
                *usage = Some(Usage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: total as usize,
                });
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parses_responses_text_reasoning_and_function_call_events() {
        let mut buf = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"think\"}\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"shell\",\"arguments\":\"{\\\"cmd\\\":\\\"pwd\\\"}\"}}\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"total_tokens\":7}}}\n",
        )
        .to_string();
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = Vec::new();
        let mut finish_reason = None;
        let mut usage = None;

        drain_sse_buffer(
            &mut buf,
            &mut content,
            &mut reasoning,
            &mut tool_calls,
            &mut finish_reason,
            &mut usage,
            None,
        )
        .await;

        assert_eq!(content, "hi");
        assert_eq!(reasoning, "think");
        assert_eq!(tool_calls[0].id, "call_1");
        assert_eq!(tool_calls[0].function.name, "shell");
        assert_eq!(usage.unwrap().total_tokens, 7);
        assert_eq!(finish_reason.as_deref(), Some("stop"));
    }
}
