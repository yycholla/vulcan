//! Todo dogfood extension for GH issue #550.
//!
//! The daemon-side extension contributes three session-local tools.
//! Runtime wiring exposes them to the model as `todo_add`,
//! `todo_list`, and `todo_clear`; internally the tools use local names
//! `add`, `list`, and `clear` so the registry owns namespacing.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use vulcan::extensions::api::{
    DaemonCodeExtension, ExtensionRegistration, SessionExtension, SessionExtensionCtx,
};
use vulcan::extensions::{
    ExtensionCapability, ExtensionMetadata, ExtensionSource, ExtensionStatus,
};
use vulcan::hooks::HookHandler;
use vulcan::provider::Message;
use vulcan::tools::{ProgressSink, ReplaySafety, Tool, ToolResult, details_from_tool_message};

const ID: &str = "todo";

#[cfg(feature = "tui")]
pub mod tui;

#[derive(Default)]
pub struct TodoExtension;

impl DaemonCodeExtension for TodoExtension {
    fn metadata(&self) -> ExtensionMetadata {
        let mut m = ExtensionMetadata::new(
            ID,
            "Todo",
            env!("CARGO_PKG_VERSION"),
            ExtensionSource::Builtin,
        );
        m.status = ExtensionStatus::Active;
        m.capabilities = vec![ExtensionCapability::ToolProvider];
        m.description = "Session-local todo list tools with ToolResult.details replay.".to_string();
        m
    }

    fn instantiate(&self, ctx: SessionExtensionCtx) -> Arc<dyn SessionExtension> {
        Arc::new(TodoSession {
            items: Arc::new(Mutex::new(Vec::new())),
            memory: ctx.memory,
        })
    }
}

struct TodoSession {
    items: Arc<Mutex<Vec<String>>>,
    memory: Arc<vulcan::memory::SessionStore>,
}

impl SessionExtension for TodoSession {
    fn hook_handlers(&self) -> Vec<Arc<dyn HookHandler>> {
        vec![Arc::new(TodoReplayHook {
            items: Arc::clone(&self.items),
            memory: Arc::clone(&self.memory),
        })]
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(TodoAddTool {
                items: Arc::clone(&self.items),
            }),
            Arc::new(TodoListTool {
                items: Arc::clone(&self.items),
            }),
            Arc::new(TodoClearTool {
                items: Arc::clone(&self.items),
            }),
        ]
    }
}

struct TodoReplayHook {
    items: Arc<Mutex<Vec<String>>>,
    memory: Arc<vulcan::memory::SessionStore>,
}

#[async_trait]
impl HookHandler for TodoReplayHook {
    fn name(&self) -> &str {
        ID
    }

    async fn session_start(&self, session_id: &str) {
        let Ok(Some(history)) = self.memory.load_history(session_id) else {
            return;
        };
        let mut latest = None;
        for msg in history {
            if let Message::Tool { content, .. } = msg {
                latest = details_from_tool_message(&content).and_then(todo_items_from_details);
            }
        }
        if let Some(items) = latest {
            *self.items.lock() = items;
        }
    }
}

fn todo_items_from_details(details: Value) -> Option<Vec<String>> {
    details
        .get("items")?
        .as_array()?
        .iter()
        .map(|v| v.as_str().map(str::to_string))
        .collect()
}

fn details(items: &[String]) -> Value {
    json!({ "items": items })
}

fn output_for(items: &[String]) -> String {
    if items.is_empty() {
        "Todo list is empty.".to_string()
    } else {
        items
            .iter()
            .enumerate()
            .map(|(idx, item)| format!("{}. {item}", idx + 1))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Deserialize)]
struct AddParams {
    item: String,
}

struct TodoAddTool {
    items: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Tool for TodoAddTool {
    fn name(&self) -> &str {
        "add"
    }

    fn description(&self) -> &str {
        "Add an item to the session todo list."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "item": { "type": "string", "description": "Todo item to add" }
            },
            "required": ["item"]
        })
    }

    async fn call(
        &self,
        params: Value,
        _cancel: CancellationToken,
        _progress: Option<ProgressSink>,
    ) -> anyhow::Result<ToolResult> {
        let p: AddParams = match vulcan::tools::parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let item = p.item.trim();
        if item.is_empty() {
            return Ok(ToolResult::err("todo item cannot be empty"));
        }
        let snapshot = {
            let mut guard = self.items.lock();
            guard.push(item.to_string());
            guard.clone()
        };
        Ok(ToolResult::ok(format!("Added todo: {item}")).with_details(details(&snapshot)))
    }

    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::Mutating
    }
}

struct TodoListTool {
    items: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Tool for TodoListTool {
    fn name(&self) -> &str {
        "list"
    }

    fn description(&self) -> &str {
        "List the current session todo items."
    }

    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn call(
        &self,
        _params: Value,
        _cancel: CancellationToken,
        _progress: Option<ProgressSink>,
    ) -> anyhow::Result<ToolResult> {
        let snapshot = self.items.lock().clone();
        Ok(ToolResult::ok(output_for(&snapshot)).with_details(details(&snapshot)))
    }

    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::ReadOnly
    }
}

struct TodoClearTool {
    items: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Tool for TodoClearTool {
    fn name(&self) -> &str {
        "clear"
    }

    fn description(&self) -> &str {
        "Clear the session todo list."
    }

    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn call(
        &self,
        _params: Value,
        _cancel: CancellationToken,
        _progress: Option<ProgressSink>,
    ) -> anyhow::Result<ToolResult> {
        self.items.lock().clear();
        Ok(ToolResult::ok("Cleared todo list.").with_details(details(&[])))
    }

    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::Mutating
    }
}

inventory::submit! {
    ExtensionRegistration {
        register: || Arc::new(TodoExtension) as Arc<dyn DaemonCodeExtension>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(memory: Arc<vulcan::memory::SessionStore>) -> SessionExtensionCtx {
        SessionExtensionCtx {
            cwd: std::path::PathBuf::from("/tmp/test"),
            session_id: "todo-test".to_string(),
            memory,
        }
    }

    #[tokio::test]
    async fn add_list_clear_emit_details_snapshots() {
        let memory = Arc::new(vulcan::memory::SessionStore::in_memory());
        let session = TodoExtension.instantiate(ctx(memory));
        let tools = session.tools();

        let add = tools.iter().find(|t| t.name() == "add").expect("add tool");
        let result = add
            .call(
                json!({ "item": "buy milk" }),
                CancellationToken::new(),
                None,
            )
            .await
            .expect("add result");
        assert_eq!(result.details, Some(json!({ "items": ["buy milk"] })));

        let list = tools
            .iter()
            .find(|t| t.name() == "list")
            .expect("list tool");
        let result = list
            .call(json!({}), CancellationToken::new(), None)
            .await
            .expect("list result");
        assert!(result.output.contains("buy milk"));
        assert_eq!(result.details, Some(json!({ "items": ["buy milk"] })));

        let clear = tools
            .iter()
            .find(|t| t.name() == "clear")
            .expect("clear tool");
        let result = clear
            .call(json!({}), CancellationToken::new(), None)
            .await
            .expect("clear result");
        assert_eq!(result.details, Some(json!({ "items": [] })));
    }
}
