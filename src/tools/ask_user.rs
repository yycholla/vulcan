//! `ask_user` tool (YYC-81). Agent-initiated multiple-choice prompt
//! over the existing `AgentPause` channel. The TUI renders inline
//! pills (YYC-59); the chosen option's `value` comes back as
//! `AgentResume::Custom` and the tool returns it to the LLM.
//!
//! Only registered when a pause emitter is wired (TUI mode); the CLI
//! one-shot path returns a clear error so the agent knows it can't
//! ask interactively there.

use crate::pause::{AgentPause, AgentResume, OptionKind, PauseKind, PauseOption, PauseSender};
use crate::tools::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub struct AskUserTool {
    pause_tx: Option<PauseSender>,
}

impl AskUserTool {
    pub fn new(pause_tx: Option<PauseSender>) -> Self {
        Self { pause_tx }
    }
}

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }
    fn description(&self) -> &str {
        "Ask the user a multiple-choice question and block until they pick. Each option has {key, label, value}; the chosen value comes back as the tool result. Only available in TUI mode."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "question": { "type": "string" },
                "options": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "key": { "type": "string", "description": "Single-char shortcut" },
                            "label": { "type": "string" },
                            "value": { "type": "string", "description": "Returned to the agent on press" }
                        },
                        "required": ["key", "label", "value"]
                    }
                }
            },
            "required": ["question", "options"]
        })
    }
    async fn call(&self, params: Value, cancel: CancellationToken) -> Result<ToolResult> {
        let question = params["question"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("question required"))?
            .to_string();
        let options_in = params["options"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("options must be a non-empty array"))?
            .clone();
        if options_in.is_empty() {
            return Ok(ToolResult::err("options must contain at least one entry"));
        }

        let tx = match &self.pause_tx {
            Some(t) => t,
            None => {
                return Ok(ToolResult::err(
                    "ask_user is only available when a pause channel is wired (TUI mode)",
                ));
            }
        };

        // Map JSON entries to PauseOption with Custom(value) resume.
        let mut options = Vec::with_capacity(options_in.len());
        for (i, entry) in options_in.iter().enumerate() {
            let key = entry["key"]
                .as_str()
                .and_then(|s| s.chars().next())
                .ok_or_else(|| anyhow::anyhow!("options[{i}].key must be a single character"))?;
            let label = entry["label"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("options[{i}].label required"))?
                .to_string();
            let value = entry["value"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("options[{i}].value required"))?
                .to_string();
            // First option = primary; rest = neutral. Lets the TUI
            // pill colors hint at the suggested default without the
            // agent having to specify per-option styling.
            let kind = if i == 0 {
                OptionKind::Primary
            } else {
                OptionKind::Neutral
            };
            options.push(PauseOption {
                key,
                label,
                kind,
                resume: AgentResume::Custom(value),
            });
        }

        let (reply_tx, reply_rx) = oneshot::channel();
        let pause = AgentPause {
            kind: PauseKind::UserChoice { question },
            reply: reply_tx,
            options,
        };

        if tx.send(pause).await.is_err() {
            return Ok(ToolResult::err(
                "ask_user: pause channel closed before the prompt could be delivered",
            ));
        }

        // Block until the user responds (or cancels). 10-minute cap
        // so a forgotten prompt eventually unblocks the agent.
        let resume = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(ToolResult::err("Cancelled")),
            r = tokio::time::timeout(Duration::from_secs(600), reply_rx) => match r {
                Err(_) => return Ok(ToolResult::err("ask_user: timed out waiting for user response")),
                Ok(Err(_)) => return Ok(ToolResult::err("ask_user: response channel closed")),
                Ok(Ok(r)) => r,
            },
        };

        match resume {
            AgentResume::Custom(v) => Ok(ToolResult::ok(v)),
            AgentResume::Allow => Ok(ToolResult::ok("allow".to_string())),
            AgentResume::AllowAndRemember => Ok(ToolResult::ok("allow_and_remember".to_string())),
            AgentResume::Deny => Ok(ToolResult::err("user denied")),
            AgentResume::DenyWithReason(r) => Ok(ToolResult::err(format!("user denied: {r}"))),
        }
    }
}
