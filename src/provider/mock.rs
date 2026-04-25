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
        self.captured_calls
            .lock()
            .unwrap()
            .push(messages.to_vec());
        let r = self.next_response()?;
        Self::build_chat_response(r)
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        tx: mpsc::UnboundedSender<StreamEvent>,
        _cancel: CancellationToken,
    ) -> Result<()> {
        self.captured_calls
            .lock()
            .unwrap()
            .push(messages.to_vec());
        let r = self.next_response()?;

        match &r {
            MockResponse::Text(s) => {
                if !s.is_empty() {
                    let _ = tx.send(StreamEvent::Text(s.clone()));
                }
            }
            MockResponse::WithReasoning { reasoning, content } => {
                if !reasoning.is_empty() {
                    let _ = tx.send(StreamEvent::Reasoning(reasoning.clone()));
                }
                if !content.is_empty() {
                    let _ = tx.send(StreamEvent::Text(content.clone()));
                }
            }
            MockResponse::Mixed { content, .. } => {
                if !content.is_empty() {
                    let _ = tx.send(StreamEvent::Text(content.clone()));
                }
            }
            MockResponse::ToolCalls(_) | MockResponse::Error(_) => {}
        }

        let response = Self::build_chat_response(r)?;
        let _ = tx.send(StreamEvent::Done(response));
        Ok(())
    }

    fn max_context(&self) -> usize {
        self.max_context
    }
}
