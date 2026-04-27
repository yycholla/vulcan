use crate::config::ProviderDebugMode;
use crate::provider::{
    ChatResponse, LLMProvider, Message, ProviderError, StreamEvent, ToolCall, ToolDefinition, Usage,
};
use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::{Value, json};
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
    /// Capability metadata from the provider catalog. This tells us the model
    /// can support structured outputs if some future caller explicitly asks
    /// for them, but normal chat/tool turns do not auto-enable
    /// `response_format`.
    _supports_json_mode: bool,
    debug_mode: ProviderDebugMode,
}

impl OpenAIProvider {
    pub fn new(
        base_url: &str,
        api_key: &str,
        model: &str,
        max_context: usize,
        max_retries: u32,
        supports_json_mode: bool,
        debug_mode: ProviderDebugMode,
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
            _supports_json_mode: supports_json_mode,
            debug_mode,
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
    /// `max_retries` times on transient failures (429, 5xx, network errors)
    /// with exponential backoff + jitter. Non-retryable errors
    /// (Auth/BadRequest/ModelNotFound/Other) and cancellation pass through
    /// immediately. Returns a structured `ProviderError` on failure so
    /// callers can render actionable messages and retry policies key off
    /// the variant rather than raw status codes.
    async fn do_request(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        cancel: &CancellationToken,
    ) -> std::result::Result<reqwest::Response, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = self.build_request(messages, tools);
        if self.debug_mode.logs_wire() {
            tracing::info!(
                provider = "openai-compat",
                model = %self.model,
                url = %url,
                request_body = %body,
                "provider wire request"
            );
        }

        let mut last_err: Option<ProviderError> = None;
        for attempt in 0..=self.max_retries {
            if cancel.is_cancelled() {
                // Surface as Other; cancellation in the API layer doesn't
                // map to a real provider failure.
                return Err(ProviderError::Other {
                    status: 0,
                    body: "Cancelled".into(),
                });
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
                    _ = cancel.cancelled() => {
                        return Err(ProviderError::Other {
                            status: 0,
                            body: "Cancelled".into(),
                        });
                    }
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

            let err = match send_result {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        return Ok(response);
                    }
                    let body_text = response.text().await.unwrap_or_default();
                    ProviderError::from_response(status, &body_text, &self.model)
                }
                Err(e) => ProviderError::Network(e),
            };

            if err.is_retryable() && attempt < self.max_retries {
                tracing::warn!(
                    "Provider error (retryable, attempt {}/{}): {}",
                    attempt + 1,
                    self.max_retries,
                    truncate(&err.to_string(), 200)
                );
                last_err = Some(err);
                continue;
            }
            // Non-retryable, or budget exhausted — surface the error.
            return Err(err);
        }

        Err(last_err.unwrap_or(ProviderError::Other {
            status: 0,
            body: "retry budget exhausted".into(),
        }))
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
                        // Reasoning trace from thinking-mode models. The
                        // field name varies by proxy — DeepSeek's native API
                        // uses `reasoning_content`, OpenRouter normalizes to
                        // `reasoning`. Accept either and merge into one
                        // accumulator (YYC-63).
                        if let Some(rc) = delta.get("reasoning_content").and_then(|c| c.as_str()) {
                            reasoning.push_str(rc);
                        }
                        if let Some(rc) = delta.get("reasoning").and_then(|c| c.as_str()) {
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
                                    if let Some(id) = tc.get("id").and_then(|i| i.as_str())
                                        && !id.is_empty() {
                                            tool_calls[idx].id.push_str(id);
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
                    if let Some(reason) = choice["finish_reason"].as_str()
                        && !reason.is_empty() && reason != "null" {
                            *finish_reason = Some(reason.to_string());
                        }
                }
            }
            if let Some(u) = chunk.get("usage")
                && let (Some(prompt), Some(completion)) = (
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

fn log_tool_fallback_if_enabled(
    debug_mode: ProviderDebugMode,
    model: &str,
    content: &str,
    finish_reason: Option<&str>,
    inferred_tool_calls: &[ToolCall],
) {
    if !debug_mode.logs_tool_fallback() || inferred_tool_calls.is_empty() {
        return;
    }

    let tool_names = inferred_tool_calls
        .iter()
        .map(|tc| tc.function.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    tracing::info!(
        provider = "openai-compat",
        model = %model,
        finish_reason = finish_reason.unwrap_or(""),
        inferred_tools = %tool_names,
        assistant_content = %content,
        "provider response used content-shaped tool-call fallback"
    );
}

#[derive(Debug, PartialEq, Eq)]
struct WireResponseSummary {
    raw_body: Option<String>,
    sse_data_lines: usize,
    has_done_marker: bool,
    non_sse_preview: Option<String>,
}

fn summarize_wire_response(raw: &str) -> WireResponseSummary {
    let mut sse_data_lines = 0;
    let mut has_done_marker = false;
    let mut non_sse_lines = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("data: ") {
            sse_data_lines += 1;
            if trimmed == "data: [DONE]" {
                has_done_marker = true;
            }
        } else {
            non_sse_lines.push(trimmed.to_string());
        }
    }

    if sse_data_lines > 0 {
        let preview =
            (!non_sse_lines.is_empty()).then(|| truncate(&non_sse_lines.join("\n"), 2_000));
        WireResponseSummary {
            raw_body: None,
            sse_data_lines,
            has_done_marker,
            non_sse_preview: preview,
        }
    } else {
        WireResponseSummary {
            raw_body: Some(raw.to_string()),
            sse_data_lines: 0,
            has_done_marker: false,
            non_sse_preview: None,
        }
    }
}

fn log_wire_response(model: &str, raw: &str) {
    let summary = summarize_wire_response(raw);
    if let Some(body) = summary.raw_body {
        tracing::info!(
            provider = "openai-compat",
            model = %model,
            response_body = %body,
            "provider wire response"
        );
    } else {
        tracing::info!(
            provider = "openai-compat",
            model = %model,
            sse_data_lines = summary.sse_data_lines,
            has_done_marker = summary.has_done_marker,
            non_sse_preview = summary.non_sse_preview.as_deref().unwrap_or(""),
            "provider wire response (stream summary; completion chunks suppressed)"
        );
    }
}

fn maybe_strip_json_fence(content: &str) -> &str {
    let trimmed = content.trim();
    if let Some(rest) = trimmed.strip_prefix("```json") {
        return rest.strip_suffix("```").unwrap_or(rest).trim();
    }
    if let Some(rest) = trimmed.strip_prefix("```") {
        return rest.strip_suffix("```").unwrap_or(rest).trim();
    }
    trimmed
}

fn missing_required_keys(schema: &Value, params: &Value) -> Option<Vec<String>> {
    let required = schema.get("required")?.as_array()?;
    let provided = params.as_object()?;
    let missing = required
        .iter()
        .filter_map(|v| v.as_str())
        .filter(|key| !provided.contains_key(*key))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    Some(missing)
}

fn score_tool_match(params: &Value, tool: &ToolDefinition) -> Option<(usize, usize)> {
    let obj = params.as_object()?;
    let schema = &tool.function.parameters;
    let missing = missing_required_keys(schema, params)?;
    if !missing.is_empty() {
        return None;
    }

    let properties = schema
        .get("properties")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let recognized = obj.keys().filter(|k| properties.contains_key(*k)).count();
    let extras = obj.len().saturating_sub(recognized);
    Some((recognized, extras))
}

fn infer_bare_object_tool_call(params: &Value, tools: &[ToolDefinition]) -> Option<ToolCall> {
    let mut best: Option<(&ToolDefinition, usize, usize)> = None;

    for tool in tools {
        let Some((recognized, extras)) = score_tool_match(params, tool) else {
            continue;
        };

        match best {
            None => best = Some((tool, recognized, extras)),
            Some((_, best_recognized, best_extras)) => {
                if recognized > best_recognized
                    || (recognized == best_recognized && extras < best_extras)
                {
                    best = Some((tool, recognized, extras));
                } else if recognized == best_recognized && extras == best_extras {
                    // Ambiguous match: don't guess.
                    best = None;
                }
            }
        }
    }

    let (tool, _, _) = best?;
    Some(ToolCall {
        id: "fallback_tool_call_0".into(),
        call_type: "function".into(),
        function: crate::provider::ToolCallFunction {
            name: tool.function.name.clone(),
            arguments: serde_json::to_string(params).ok()?,
        },
    })
}

fn infer_content_tool_calls(content: &str, tools: &[ToolDefinition]) -> Option<Vec<ToolCall>> {
    if tools.is_empty() {
        return None;
    }

    let trimmed = maybe_strip_json_fence(content);
    let value: Value = serde_json::from_str(trimmed).ok()?;

    if let Some(obj) = value.as_object() {
        if let Some(tool_calls) = obj.get("tool_calls").and_then(|v| v.as_array()) {
            let parsed = tool_calls
                .iter()
                .enumerate()
                .filter_map(|(idx, call)| {
                    let call_obj = call.as_object()?;
                    let function = call_obj.get("function")?.as_object()?;
                    let name = function.get("name")?.as_str()?.to_string();
                    let arguments = function.get("arguments")?;
                    let arguments = if let Some(s) = arguments.as_str() {
                        s.to_string()
                    } else {
                        serde_json::to_string(arguments).ok()?
                    };
                    Some(ToolCall {
                        id: call_obj
                            .get("id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("fallback_tool_call_{idx}")),
                        call_type: "function".into(),
                        function: crate::provider::ToolCallFunction { name, arguments },
                    })
                })
                .collect::<Vec<_>>();
            if !parsed.is_empty() {
                return Some(parsed);
            }
        }

        if let Some(arguments) = obj.get("arguments").or_else(|| obj.get("params"))
            && let Some(name) = obj
                .get("name")
                .or_else(|| obj.get("tool"))
                .or_else(|| obj.get("tool_name"))
                .and_then(|v| v.as_str())
            {
                let arguments = if let Some(s) = arguments.as_str() {
                    s.to_string()
                } else {
                    serde_json::to_string(arguments).ok()?
                };
                return Some(vec![ToolCall {
                    id: "fallback_tool_call_0".into(),
                    call_type: "function".into(),
                    function: crate::provider::ToolCallFunction {
                        name: name.to_string(),
                        arguments,
                    },
                }]);
            }
    }

    infer_bare_object_tool_call(&value, tools).map(|tc| vec![tc])
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
        if self.debug_mode.logs_wire() {
            log_wire_response(&self.model, &text);
        }

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

        // YYC-103: extract any inline `<think>…</think>` blocks from the
        // accumulated content and route them to the reasoning trace.
        let mut sanitizer = crate::provider::think_sanitizer::ThinkSanitizer::new();
        let sanitized = sanitizer.feed(&content);
        let tail = sanitizer.flush();
        content = format!("{}{}", sanitized.text, tail.text);
        if !sanitized.reasoning.is_empty() {
            reasoning.push_str(&sanitized.reasoning);
        }
        if !tail.reasoning.is_empty() {
            reasoning.push_str(&tail.reasoning);
        }

        let inferred_tool_calls = if tool_calls.is_empty() {
            infer_content_tool_calls(&content, tools)
        } else {
            None
        };
        if let Some(inferred) = inferred_tool_calls.as_ref() {
            log_tool_fallback_if_enabled(
                self.debug_mode,
                &self.model,
                &content,
                finish_reason.as_deref(),
                inferred,
            );
        }
        let content = if inferred_tool_calls.is_some() {
            String::new()
        } else {
            content
        };

        Ok(ChatResponse {
            content: Some(content).filter(|c| !c.is_empty()),
            tool_calls: inferred_tool_calls.or_else(|| Some(tool_calls).filter(|c| !c.is_empty())),
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
        let mut raw_stream = self.debug_mode.logs_wire().then(String::new);
        // YYC-103: split inline `<think>` blocks out of the visible content
        // stream as they arrive.
        let mut think = crate::provider::think_sanitizer::ThinkSanitizer::new();

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
            if let Some(raw) = raw_stream.as_mut() {
                raw.push_str(&chunk_str);
            }
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

                // YYC-103: run the just-appended content delta through
                // the think-tag sanitizer so `<think>…</think>` blocks
                // route to the reasoning channel instead of leaking
                // into chat. Native reasoning_content (DeepSeek /
                // OpenRouter) still flows through unchanged.
                let raw_content_delta = content[prev_content_len..].to_string();
                content.truncate(prev_content_len);
                let sanitized = think.feed(&raw_content_delta);
                content.push_str(&sanitized.text);
                let native_reasoning_delta = reasoning[prev_reasoning_len..].to_string();
                reasoning.push_str(&sanitized.reasoning);

                if !sanitized.text.is_empty() {
                    let _ = tx.send(StreamEvent::Text(sanitized.text));
                }
                let mut combined_reasoning = native_reasoning_delta;
                combined_reasoning.push_str(&sanitized.reasoning);
                // YYC-103: empty `<think></think>` blocks stream `\n` or
                // whitespace; suppress those so a stub THINKING header
                // doesn't get inserted between text segments.
                if !combined_reasoning.trim().is_empty() {
                    let _ = tx.send(StreamEvent::Reasoning(combined_reasoning));
                }
            }
        }
        // Drain any pending partial-tag buffer left at stream close.
        let leftover = think.flush();
        if !leftover.text.is_empty() {
            content.push_str(&leftover.text);
            let _ = tx.send(StreamEvent::Text(leftover.text));
        }
        if !leftover.reasoning.trim().is_empty() {
            reasoning.push_str(&leftover.reasoning);
            let _ = tx.send(StreamEvent::Reasoning(leftover.reasoning));
        }

        if let Some(raw) = raw_stream.as_ref() {
            log_wire_response(&self.model, raw);
        }

        let inferred_tool_calls = if tool_calls.is_empty() {
            infer_content_tool_calls(&content, tools)
        } else {
            None
        };
        if let Some(inferred) = inferred_tool_calls.as_ref() {
            log_tool_fallback_if_enabled(
                self.debug_mode,
                &self.model,
                &content,
                finish_reason.as_deref(),
                inferred,
            );
        }
        let content = if inferred_tool_calls.is_some() {
            String::new()
        } else {
            content
        };

        // Build final ChatResponse and signal done
        let response = ChatResponse {
            content: Some(content).filter(|c| !c.is_empty()),
            tool_calls: inferred_tool_calls.or_else(|| Some(tool_calls).filter(|c| !c.is_empty())),
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
    use crate::config::ProviderDebugMode;
    use crate::provider::ProviderError;
    use reqwest::StatusCode;
    use serde_json::json;

    fn classify(code: u16, body: &str) -> ProviderError {
        ProviderError::from_response(
            StatusCode::from_u16(code).unwrap(),
            body,
            "deepseek/deepseek-v4-flash",
        )
    }

    #[test]
    fn classifier_handles_common_provider_errors() {
        // 401 / 403 → Auth
        assert!(matches!(
            classify(401, r#"{"error":{"message":"User not found.","code":401}}"#),
            ProviderError::Auth { .. }
        ));
        assert!(matches!(
            classify(403, r#"{"error":{"message":"Forbidden"}}"#),
            ProviderError::Auth { .. }
        ));
        // 429 → RateLimited
        assert!(matches!(
            classify(429, r#"{"error":{"message":"slow down"}}"#),
            ProviderError::RateLimited { .. }
        ));
        // 400 / 422 → BadRequest
        assert!(matches!(
            classify(400, r#"{"error":{"message":"bad shape"}}"#),
            ProviderError::BadRequest { .. }
        ));
        assert!(matches!(
            classify(422, r#"{"error":{"message":"invalid arg"}}"#),
            ProviderError::BadRequest { .. }
        ));
        // 5xx → ServerError
        assert!(matches!(
            classify(500, r#"{"error":{"message":"oops"}}"#),
            ProviderError::ServerError { .. }
        ));
        assert!(matches!(
            classify(503, r#"{"error":{"message":"down"}}"#),
            ProviderError::ServerError { .. }
        ));
        // 404 with model in body → ModelNotFound
        assert!(matches!(
            classify(
                404,
                r#"{"error":{"message":"Model deepseek/deepseek-v4-flash not found"}}"#
            ),
            ProviderError::ModelNotFound { .. }
        ));
        // 404 without model hint → Other
        assert!(matches!(
            classify(404, r#"{"error":{"message":"Resource gone"}}"#),
            ProviderError::Other { .. }
        ));
        // Non-JSON body → Other (or BadRequest depending on status)
        let html = "<html>500 internal server error</html>";
        assert!(matches!(
            classify(500, html),
            ProviderError::ServerError { .. }
        ));
    }

    #[test]
    fn classifier_unwraps_openrouter_nested_error() {
        // OpenRouter wraps the upstream provider's body inside metadata.raw.
        let body = r#"{"error":{"message":"Provider returned error","code":400,"metadata":{"raw":"{\"error\":{\"message\":\"The reasoning_content in the thinking mode must be passed back to the API.\"}}"}}}"#;
        let err = classify(400, body);
        match err {
            ProviderError::BadRequest { message } => {
                assert!(
                    message.contains("reasoning_content"),
                    "expected unwrapped DeepSeek message, got {message:?}"
                );
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    #[test]
    fn provider_error_retry_classification() {
        assert!(classify(429, "{}").is_retryable());
        assert!(classify(500, "{}").is_retryable());
        assert!(classify(502, "{}").is_retryable());
        assert!(classify(503, "{}").is_retryable());
        // Auth/BadRequest/ModelNotFound/Other are not.
        assert!(!classify(401, r#"{"error":{"message":"x"}}"#).is_retryable());
        assert!(!classify(400, r#"{"error":{"message":"x"}}"#).is_retryable());
        assert!(
            !classify(
                404,
                r#"{"error":{"message":"Model deepseek/deepseek-v4-flash not found"}}"#
            )
            .is_retryable()
        );
    }

    #[test]
    fn provider_error_display_includes_actionable_hint() {
        let auth = classify(401, r#"{"error":{"message":"User not found."}}"#).to_string();
        assert!(auth.contains("API key"), "got {auth:?}");
        let model = classify(
            404,
            r#"{"error":{"message":"Model deepseek/deepseek-v4-flash not found"}}"#,
        )
        .to_string();
        assert!(model.contains("provider's catalog"), "got {model:?}");
    }

    fn test_provider(supports_json_mode: bool) -> OpenAIProvider {
        OpenAIProvider::new(
            "https://example.com/v1",
            "test-key",
            "test-model",
            128_000,
            0,
            supports_json_mode,
            ProviderDebugMode::Off,
        )
        .expect("provider")
    }

    #[test]
    fn build_request_omits_response_format_for_normal_tool_requests_even_with_json_mode() {
        let provider = test_provider(true);
        let body = provider.build_request(
            &[Message::User {
                content: "hi".to_string(),
            }],
            &[ToolDefinition {
                tool_type: "function".into(),
                function: crate::provider::ToolFunction {
                    name: "read_file".into(),
                    description: "Read a file".into(),
                    parameters: json!({
                        "type": "object",
                        "properties": { "path": { "type": "string" } },
                        "required": ["path"]
                    }),
                },
            }],
        );

        assert!(body.get("tools").is_some(), "expected tools in request");
        assert!(
            body.get("response_format").is_none(),
            "normal tool turns should not force structured-output mode: {body:?}"
        );
    }

    #[test]
    fn build_request_omits_response_format_without_tools_or_support() {
        let with_support = test_provider(true);
        let no_tools = with_support.build_request(
            &[Message::User {
                content: "hi".to_string(),
            }],
            &[],
        );
        assert!(
            no_tools.get("response_format").is_none(),
            "got {no_tools:?}"
        );

        let without_support = test_provider(false);
        let no_support = without_support.build_request(
            &[Message::User {
                content: "hi".to_string(),
            }],
            &[ToolDefinition {
                tool_type: "function".into(),
                function: crate::provider::ToolFunction {
                    name: "read_file".into(),
                    description: "Read a file".into(),
                    parameters: json!({
                        "type": "object",
                        "properties": { "path": { "type": "string" } },
                        "required": ["path"]
                    }),
                },
            }],
        );
        assert!(
            no_support.get("response_format").is_none(),
            "got {no_support:?}"
        );
    }

    #[test]
    fn content_fallback_infers_bash_tool_call_from_bare_json_args() {
        let tools = vec![
            ToolDefinition {
                tool_type: "function".into(),
                function: crate::provider::ToolFunction {
                    name: "bash".into(),
                    description: "Run a shell command".into(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "command": { "type": "string" },
                            "timeout": { "type": "integer" },
                            "workdir": { "type": "string" }
                        },
                        "required": ["command"]
                    }),
                },
            },
            ToolDefinition {
                tool_type: "function".into(),
                function: crate::provider::ToolFunction {
                    name: "read_file".into(),
                    description: "Read a file".into(),
                    parameters: json!({
                        "type": "object",
                        "properties": { "path": { "type": "string" } },
                        "required": ["path"]
                    }),
                },
            },
        ];

        let inferred =
            infer_content_tool_calls(r#"{"command":"ls -la","dependencies":[]}"#, &tools)
                .expect("expected fallback tool call");
        assert_eq!(inferred.len(), 1);
        assert_eq!(inferred[0].function.name, "bash");
        assert_eq!(
            serde_json::from_str::<Value>(&inferred[0].function.arguments).unwrap(),
            json!({"command":"ls -la","dependencies":[]})
        );
    }

    #[test]
    fn wire_response_summary_suppresses_sse_completion_chunks() {
        let raw = r#"data: {"id":"gen-1","choices":[{"delta":{"content":"   ","role":"assistant"}}]}

data: {"id":"gen-1","choices":[{"delta":{"content":"{\"","role":"assistant"}}]}

data: [DONE]
"#;

        let summary = summarize_wire_response(raw);
        assert_eq!(
            summary,
            WireResponseSummary {
                raw_body: None,
                sse_data_lines: 3,
                has_done_marker: true,
                non_sse_preview: None,
            }
        );
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
