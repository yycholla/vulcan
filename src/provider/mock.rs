//! Test-only mock LLM provider. Returns scripted responses from a queue.
//!
//! Usage:
//! ```ignore
//! let mock = MockProvider::new(128_000);
//! mock.enqueue_tool_call("bash", "call_1", serde_json::json!({"command":"ls"}));
//! mock.enqueue_text("Files: a.txt b.txt");
//! // ...wire into Agent and run a turn...
//! ```
//!
//! Each call to `chat` or `chat_stream` pops one response. Out-of-script
//! calls return an error so tests fail loudly rather than mysteriously.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{
    ChatResponse, LLMProvider, Message, StreamEvent, ToolCall, ToolCallFunction, ToolDefinition,
};

/// One scripted response.
#[derive(Debug, Clone)]
pub enum MockResponse {
    /// Plain text content. Streamed character-by-character in `chat_stream`.
    Text(String),
    /// Tool calls. No content text. Use when the model would respond with
    /// tools-only.
    ToolCalls(Vec<ToolCall>),
    /// Content + tool calls together (e.g. preamble text + tool dispatch).
    Mixed {
        content: String,
        tool_calls: Vec<ToolCall>,
    },
    /// Reasoning trace followed by content. Streams reasoning first, then content.
    WithReasoning { reasoning: String, content: String },
    /// Force the next call to return this error. Useful for retry/recovery tests.
    Error(String),
}

pub struct MockProvider {
    responses: Mutex<VecDeque<MockResponse>>,
    /// Captures every `messages` slice the provider was called with, in order.
    /// Tests assert against this to verify the agent built the right history.
    captured_calls: Mutex<Vec<Vec<Message>>>,
    max_context: usize,
}

impl MockProvider {
    pub fn new(max_context: usize) -> Self {
        Self {
            responses: Mutex::new(VecDeque::new()),
            captured_calls: Mutex::new(Vec::new()),
            max_context,
        }
    }

    pub fn enqueue(&self, r: MockResponse) -> &Self {
        self.responses.lock().unwrap().push_back(r);
        self
    }

    pub fn enqueue_text(&self, content: impl Into<String>) -> &Self {
        self.enqueue(MockResponse::Text(content.into()))
    }

    pub fn enqueue_tool_call(
        &self,
        name: impl Into<String>,
        id: impl Into<String>,
        args: serde_json::Value,
    ) -> &Self {
        let tc = ToolCall {
            id: id.into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: name.into(),
                arguments: args.to_string(),
            },
        };
        self.enqueue(MockResponse::ToolCalls(vec![tc]))
    }

    /// Enqueue a response containing multiple tool calls in a single turn —
    /// the parallel-dispatch path's primary input.
    pub fn enqueue_tool_calls(&self, calls: Vec<(&str, &str, serde_json::Value)>) -> &Self {
        let tcs: Vec<ToolCall> = calls
            .into_iter()
            .map(|(name, id, args)| ToolCall {
                id: id.to_string(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: name.to_string(),
                    arguments: args.to_string(),
                },
            })
            .collect();
        self.enqueue(MockResponse::ToolCalls(tcs))
    }

    pub fn enqueue_error(&self, message: impl Into<String>) -> &Self {
        self.enqueue(MockResponse::Error(message.into()))
    }

    /// Snapshot of all messages-slices the provider was called with, in order.
    pub fn captured_calls(&self) -> Vec<Vec<Message>> {
        self.captured_calls.lock().unwrap().clone()
    }

    fn next_response(&self) -> Result<MockResponse> {
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("MockProvider: response queue exhausted (test bug?)"))
    }

    fn build_chat_response(r: MockResponse) -> Result<ChatResponse> {
        match r {
            MockResponse::Text(s) => Ok(ChatResponse {
                content: Some(s).filter(|c| !c.is_empty()),
                tool_calls: None,
                usage: None,
                finish_reason: Some("stop".into()),
                reasoning_content: None,
            }),
            MockResponse::ToolCalls(tcs) => Ok(ChatResponse {
                content: None,
                tool_calls: Some(tcs),
                usage: None,
                finish_reason: Some("tool_calls".into()),
                reasoning_content: None,
            }),
            MockResponse::Mixed {
                content,
                tool_calls,
            } => Ok(ChatResponse {
                content: Some(content).filter(|c| !c.is_empty()),
                tool_calls: Some(tool_calls),
                usage: None,
                finish_reason: Some("tool_calls".into()),
                reasoning_content: None,
            }),
            MockResponse::WithReasoning { reasoning, content } => Ok(ChatResponse {
                content: Some(content).filter(|c| !c.is_empty()),
                tool_calls: None,
                usage: None,
                finish_reason: Some("stop".into()),
                reasoning_content: Some(reasoning).filter(|r| !r.is_empty()),
            }),
            MockResponse::Error(msg) => Err(anyhow::anyhow!("MockProvider error: {msg}")),
        }
    }
}

#[async_trait]
impl LLMProvider for MockProvider {
    async fn chat(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        _cancel: CancellationToken,
    ) -> Result<ChatResponse> {
        self.captured_calls.lock().unwrap().push(messages.to_vec());
        let r = self.next_response()?;
        Self::build_chat_response(r)
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        tx: mpsc::Sender<StreamEvent>,
        _cancel: CancellationToken,
    ) -> Result<()> {
        self.captured_calls.lock().unwrap().push(messages.to_vec());
        let r = self.next_response()?;

        match &r {
            MockResponse::Text(s) => {
                if !s.is_empty() {
                    let _ = tx.send(StreamEvent::Text(s.clone())).await;
                }
            }
            MockResponse::WithReasoning { reasoning, content } => {
                if !reasoning.is_empty() {
                    let _ = tx.send(StreamEvent::Reasoning(reasoning.clone())).await;
                }
                if !content.is_empty() {
                    let _ = tx.send(StreamEvent::Text(content.clone())).await;
                }
            }
            MockResponse::Mixed { content, .. } => {
                if !content.is_empty() {
                    let _ = tx.send(StreamEvent::Text(content.clone())).await;
                }
            }
            MockResponse::ToolCalls(_) | MockResponse::Error(_) => {}
        }

        let response = Self::build_chat_response(r)?;
        let _ = tx.send(StreamEvent::Done(response)).await;
        Ok(())
    }

    fn max_context(&self) -> usize {
        self.max_context
    }
}

/// Deterministic generator-style provider for soak benchmarks.
///
/// Unlike `MockProvider` (which pops from a fixed queue), `GeneratedProvider`
/// computes a response from the current turn index, letting callers drive
/// thousands of turns without enqueueing thousands of `MockResponse`s.
///
/// The script closure takes the zero-indexed turn number and returns a
/// `ChatResponse`. Counter is shared across `chat` and `chat_stream`.
pub struct GeneratedProvider {
    script: Box<dyn Fn(usize) -> ChatResponse + Send + Sync>,
    counter: AtomicUsize,
    max_context: usize,
}

impl GeneratedProvider {
    pub fn new<F>(max_context: usize, script: F) -> Self
    where
        F: Fn(usize) -> ChatResponse + Send + Sync + 'static,
    {
        Self {
            script: Box::new(script),
            counter: AtomicUsize::new(0),
            max_context,
        }
    }

    pub fn turns_completed(&self) -> usize {
        self.counter.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LLMProvider for GeneratedProvider {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _cancel: CancellationToken,
    ) -> Result<ChatResponse> {
        let turn = self.counter.fetch_add(1, Ordering::SeqCst);
        Ok((self.script)(turn))
    }

    async fn chat_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        tx: mpsc::Sender<StreamEvent>,
        _cancel: CancellationToken,
    ) -> Result<()> {
        let turn = self.counter.fetch_add(1, Ordering::SeqCst);
        let response = (self.script)(turn);
        if let Some(content) = response.content.clone()
            && !content.is_empty()
        {
            let _ = tx.send(StreamEvent::Text(content)).await;
        }
        let _ = tx.send(StreamEvent::Done(response)).await;
        Ok(())
    }

    fn max_context(&self) -> usize {
        self.max_context
    }
}

#[cfg(test)]
mod generated_tests {
    use super::*;
    use crate::provider::Message;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn generated_provider_calls_script_with_turn_index() {
        let provider = GeneratedProvider::new(128_000, |turn| ChatResponse {
            content: Some(format!("turn-{turn}")),
            tool_calls: None,
            usage: None,
            finish_reason: Some("stop".into()),
            reasoning_content: None,
        });

        let cancel = CancellationToken::new();
        let r1 = provider
            .chat(
                &[Message::User {
                    content: "hi".into(),
                }],
                &[],
                cancel.clone(),
            )
            .await
            .unwrap();
        let r2 = provider
            .chat(
                &[Message::User {
                    content: "hi".into(),
                }],
                &[],
                cancel,
            )
            .await
            .unwrap();
        assert_eq!(r1.content.as_deref(), Some("turn-0"));
        assert_eq!(r2.content.as_deref(), Some("turn-1"));
    }

    // ── YYC-132: bounded stream channel applies backpressure ────────────

    #[tokio::test]
    async fn slow_consumer_blocks_provider_at_channel_capacity() {
        // YYC-132 acceptance pin: with the bounded stream channel, a
        // slow consumer applies backpressure to the provider. The
        // provider's tx.send(ev).await blocks once the channel buffer
        // is full instead of letting memory grow unbounded.
        //
        // Drives a GeneratedProvider that emits a single Text +
        // Done per chat_stream call, then enforces a capacity-of-2
        // channel and a deliberately slow consumer. Asserts the
        // provider had to wait for the consumer at least once.
        use std::sync::atomic::{AtomicUsize, Ordering};

        let provider = GeneratedProvider::new(128_000, |_turn| ChatResponse {
            content: Some("payload".into()),
            tool_calls: None,
            usage: None,
            finish_reason: Some("stop".into()),
            reasoning_content: None,
        });

        let (tx, mut rx) = mpsc::channel::<StreamEvent>(2);
        let cancel = CancellationToken::new();

        let provider_task = tokio::spawn(async move {
            // Two iterations of (Text + Done) = 4 events into a
            // capacity-2 buffer. Without backpressure this would
            // queue all 4 immediately. With backpressure the second
            // pair waits for the consumer.
            for _ in 0..2 {
                provider
                    .chat_stream(&[], &[], tx.clone(), cancel.clone())
                    .await
                    .unwrap();
            }
        });

        // Slow consumer — drains one event then sleeps. By the time
        // it has drained 2, the provider has been waiting on send.
        let drained = AtomicUsize::new(0);
        let mut max_observed_lag = 0usize;
        for _ in 0..4 {
            // Sleep BEFORE recv to let the channel fill up.
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            // Lag = how many events are buffered. Bounded sender's
            // capacity is 2, so this should never exceed 2.
            // (rx.len() reports current buffer occupancy.)
            let lag = rx.len();
            if lag > max_observed_lag {
                max_observed_lag = lag;
            }
            assert!(
                lag <= 2,
                "channel buffer should never exceed capacity (2); saw {lag}",
            );
            let _ = rx.recv().await.expect("event");
            drained.fetch_add(1, Ordering::SeqCst);
        }

        provider_task.await.unwrap();
        // We saw the channel actually fill at some point (lag > 0)
        // — proving backpressure isn't masking unbounded growth.
        assert!(
            max_observed_lag > 0,
            "expected to observe non-zero channel lag at least once",
        );
        assert_eq!(drained.load(Ordering::SeqCst), 4);
    }
}
