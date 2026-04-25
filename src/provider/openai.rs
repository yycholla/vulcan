use crate::provider::{ChatResponse, LLMProvider, Message, StreamEvent, ToolCall, ToolDefinition, Usage};
use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// OpenAI-compatible provider (works with OpenRouter, Anthropic, Ollama, etc.)
pub struct OpenAIProvider {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
    max_context: usize,
    max_retries: u32,
}

impl OpenAIProvider {
    pub fn new(
        base_url: &str,
        api_key: &str,
        model: &str,
        max_context: usize,
        max_retries: u32,
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
            max_retries,
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

    /// Make the HTTP request and return the response. Retries up to
    /// `max_retries` times on transient failures (429, 408, 5xx, network
    /// errors) with exponential backoff + jitter. Non-retryable errors
    /// (400, 401, 403, 404, 422) and cancellation pass through immediately.
    async fn do_request(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        cancel: &CancellationToken,
    ) -> Result<reqwest::Response> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = self.build_request(messages, tools);

        for attempt in 0..=self.max_retries {
            if cancel.is_cancelled() {
                anyhow::bail!("Cancelled");
            }

            if attempt > 0 {
                let delay = backoff_delay(attempt);
                tracing::warn!(
                    "Retrying API request (attempt {}/{}) after {:?}",
                    attempt,
                    self.max_retries,
                    delay
                );
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => anyhow::bail!("Cancelled"),
                    _ = tokio::time::sleep(delay) => {},
                }
            }

            let send_result = self
                .client
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await;

            match send_result {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        return Ok(response);
                    }
                    let body_text = response.text().await.unwrap_or_default();
                    if is_retryable_status(status) && attempt < self.max_retries {
                        tracing::warn!(
                            "API returned {} (retryable, attempt {}/{}): {}",
                            status,
                            attempt + 1,
                            self.max_retries,
                            truncate(&body_text, 200)
                        );
                        continue;
                    }
                    anyhow::bail!("LLM API returned {status}: {body_text}");
                }
                Err(e) => {
                    // Network/timeout errors are always retryable within budget.
                    if attempt < self.max_retries {
                        tracing::warn!(
                            "Network error (retryable, attempt {}/{}): {}",
                            attempt + 1,
                            self.max_retries,
                            e
                        );
                        continue;
                    }
                    return Err(anyhow::Error::from(e).context("LLM API request failed"));
                }
            }
        }

        // Should be unreachable — the loop body always either returns/bails or continues.
        anyhow::bail!("LLM API retry budget exhausted")
    }

    /// Parse a single SSE data line and accumulate state
    fn parse_line(
        line: &str,
        content: &mut String,
        reasoning: &mut String,
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
                        // DeepSeek-shape reasoning trace. OpenRouter passes
                        // it through for thinking-mode models; we accumulate
                        // and echo it back on the next turn (YYC-43).
                        if let Some(rc) = delta.get("reasoning_content").and_then(|c| c.as_str()) {
                            reasoning.push_str(rc);
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
        cancel: CancellationToken,
    ) -> Result<ChatResponse> {
        let bytes = tokio::select! {
            biased;
            _ = cancel.cancelled() => anyhow::bail!("Cancelled"),
            res = async {
                let response = self.do_request(messages, tools, &cancel).await?;
                response.bytes().await.context("Failed to read response body")
            } => res?,
        };
        let text = String::from_utf8_lossy(&bytes);

        let mut content = String::new();
        let mut reasoning = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut usage: Option<Usage> = None;
        let mut finish_reason: Option<String> = None;

        for line in text.lines() {
            Self::parse_line(
                line,
                &mut content,
                &mut reasoning,
                &mut tool_calls,
                &mut usage,
                &mut finish_reason,
            );
        }

        Ok(ChatResponse {
            content: Some(content).filter(|c| !c.is_empty()),
            tool_calls: Some(tool_calls).filter(|c| !c.is_empty()),
            usage,
            finish_reason,
            reasoning_content: Some(reasoning).filter(|r| !r.is_empty()),
        })
    }

    /// Streaming chat — sends text tokens through the channel as they arrive
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        tx: mpsc::UnboundedSender<StreamEvent>,
        cancel: CancellationToken,
    ) -> Result<()> {
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => anyhow::bail!("Cancelled"),
            res = self.do_request(messages, tools, &cancel) => res?,
        };

        let mut content = String::new();
        let mut reasoning = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut usage: Option<Usage> = None;
        let mut finish_reason: Option<String> = None;

        // Read the HTTP response as a byte stream — chunks arrive as the LLM generates
        let mut stream = response.bytes_stream();
        let mut buf = String::new();

        use futures_util::StreamExt;
        loop {
            let chunk_result = tokio::select! {
                biased;
                _ = cancel.cancelled() => anyhow::bail!("Cancelled"),
                next = stream.next() => match next {
                    Some(r) => r,
                    None => break,
                },
            };
            let chunk = chunk_result.context("Failed to read stream chunk")?;
            let chunk_str = String::from_utf8_lossy(&chunk);
            buf.push_str(&chunk_str);

            // Process complete lines from the buffer
            while let Some(newline) = buf.find('\n') {
                let line = buf[..newline].to_string();
                buf = buf[newline + 1..].to_string();

                let prev_content_len = content.len();
                let prev_reasoning_len = reasoning.len();
                Self::parse_line(
                    &line,
                    &mut content,
                    &mut reasoning,
                    &mut tool_calls,
                    &mut usage,
                    &mut finish_reason,
                );

                // Send any new content/reasoning deltas through the channel.
                if content.len() > prev_content_len {
                    let delta = &content[prev_content_len..];
                    let _ = tx.send(StreamEvent::Text(delta.to_string()));
                }
                if reasoning.len() > prev_reasoning_len {
                    let delta = &reasoning[prev_reasoning_len..];
                    let _ = tx.send(StreamEvent::Reasoning(delta.to_string()));
                }
            }
        }

        // Build final ChatResponse and signal done
        let response = ChatResponse {
            content: Some(content).filter(|c| !c.is_empty()),
            tool_calls: Some(tool_calls).filter(|c| !c.is_empty()),
            usage,
            finish_reason,
            reasoning_content: Some(reasoning).filter(|r| !r.is_empty()),
        };

        let _ = tx.send(StreamEvent::Done(response));
        Ok(())
    }

    fn max_context(&self) -> usize {
        self.max_context
    }
}

/// Status codes worth retrying. Excludes auth/permission/validation errors
/// where retrying just spends more of the budget on a guaranteed failure.
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(
        status.as_u16(),
        408 | 425 | 429 | 500 | 502 | 503 | 504
    )
}

/// Exponential backoff with jitter. attempt=1 → ~1s, 2 → ~2s, 3 → ~4s, 4 → ~8s, 5 → ~16s.
/// Jitter is 0-25% of the base delay, derived from monotonic-ish system time
/// to avoid synchronized retry storms across multiple in-flight requests.
fn backoff_delay(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(4);
    let base_ms: u64 = 1_000_u64 << shift;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let jitter_window = (base_ms / 4).max(1);
    let jitter_ms = nanos % jitter_window;
    Duration::from_millis(base_ms + jitter_ms)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn retryable_includes_429_5xx_and_timeouts() {
        assert!(is_retryable_status(StatusCode::REQUEST_TIMEOUT));
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_retryable_status(StatusCode::BAD_GATEWAY));
        assert!(is_retryable_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(is_retryable_status(StatusCode::GATEWAY_TIMEOUT));
    }

    #[test]
    fn retryable_excludes_auth_and_validation_errors() {
        // These mean "your request is wrong" — retrying just burns budget.
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
        assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
        assert!(!is_retryable_status(StatusCode::FORBIDDEN));
        assert!(!is_retryable_status(StatusCode::NOT_FOUND));
        assert!(!is_retryable_status(StatusCode::UNPROCESSABLE_ENTITY));
    }

    #[test]
    fn backoff_grows_exponentially_within_cap() {
        let d1 = backoff_delay(1).as_millis();
        let d2 = backoff_delay(2).as_millis();
        let d3 = backoff_delay(3).as_millis();
        let d4 = backoff_delay(4).as_millis();
        let d5 = backoff_delay(5).as_millis();
        let d6 = backoff_delay(6).as_millis();

        // Base values: 1000, 2000, 4000, 8000, 16000, 16000 (capped at shift=4).
        // Jitter is up to 25% of base, so each delay is in [base, base + base/4).
        assert!((1000..1250).contains(&d1), "got {d1}");
        assert!((2000..2500).contains(&d2), "got {d2}");
        assert!((4000..5000).contains(&d3), "got {d3}");
        assert!((8000..10000).contains(&d4), "got {d4}");
        assert!((16000..20000).contains(&d5), "got {d5}");
        assert!((16000..20000).contains(&d6), "got {d6} (should be capped)");
    }
}
