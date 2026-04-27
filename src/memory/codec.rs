//! Encode/decode `provider::Message` rows for the SQLite session store.
//!
//! Split out of `memory/mod.rs` so the (de)serialization helpers don't
//! share a file with the `SessionStore` CRUD surface (YYC-111).

use anyhow::{Context, Result};

use crate::provider::{Message, ToolCall};

pub(in crate::memory) type Encoded = (
    &'static str,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

pub(in crate::memory) fn encode_message(msg: &Message) -> Result<Encoded> {
    Ok(match msg {
        Message::System { content } => ("system", Some(content.clone()), None, None, None),
        Message::User { content } => ("user", Some(content.clone()), None, None, None),
        Message::Assistant {
            content,
            tool_calls,
            reasoning_content,
        } => {
            let tool_calls_json = match tool_calls {
                Some(tcs) => Some(serde_json::to_string(tcs).context("encode tool_calls")?),
                None => None,
            };
            (
                "assistant",
                content.clone(),
                None,
                tool_calls_json,
                reasoning_content.clone(),
            )
        }
        Message::Tool {
            tool_call_id,
            content,
        } => (
            "tool",
            Some(content.clone()),
            Some(tool_call_id.clone()),
            None,
            None,
        ),
    })
}

pub(in crate::memory) fn decode_message(
    role: &str,
    content: Option<String>,
    tool_call_id: Option<String>,
    tool_calls_json: Option<String>,
    reasoning_content: Option<String>,
) -> Result<Message> {
    Ok(match role {
        "system" => Message::System {
            content: content.unwrap_or_default(),
        },
        "user" => Message::User {
            content: content.unwrap_or_default(),
        },
        "assistant" => {
            let tool_calls = match tool_calls_json {
                Some(s) => {
                    Some(serde_json::from_str::<Vec<ToolCall>>(&s).context("decode tool_calls")?)
                }
                None => None,
            };
            Message::Assistant {
                content,
                tool_calls,
                reasoning_content,
            }
        }
        "tool" => Message::Tool {
            tool_call_id: tool_call_id.unwrap_or_default(),
            content: content.unwrap_or_default(),
        },
        other => anyhow::bail!("unknown role in DB: {other}"),
    })
}
