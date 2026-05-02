//! Spinner status-widget demo extension for GH issue #559.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use vulcan::extensions::api::{
    DaemonCodeExtension, ExtensionRegistration, SessionExtension, SessionExtensionCtx,
};
use vulcan::extensions::{
    ExtensionCapability, ExtensionMetadata, ExtensionSource, ExtensionStatus, FrontendCapability,
};
use vulcan::tools::{ProgressSink, Tool, ToolResult};

const ID: &str = "spinner-demo";
const WIDGET_ID: &str = "long_task";

#[cfg(feature = "tui")]
pub mod tui;

#[derive(Default)]
pub struct SpinnerDemoExtension;

impl DaemonCodeExtension for SpinnerDemoExtension {
    fn metadata(&self) -> ExtensionMetadata {
        let mut meta = ExtensionMetadata::new(
            ID,
            "Spinner Demo",
            env!("CARGO_PKG_VERSION"),
            ExtensionSource::Builtin,
        );
        meta.status = ExtensionStatus::Active;
        meta.capabilities = vec![ExtensionCapability::ToolProvider];
        meta.requires_frontend = vec![FrontendCapability::StatusWidgets];
        meta.description =
            "Demo long task that drives a frontend status widget over extension events.".into();
        meta
    }

    fn instantiate(&self, ctx: SessionExtensionCtx) -> Arc<dyn SessionExtension> {
        Arc::new(SpinnerDemoSession { ctx })
    }
}

struct SpinnerDemoSession {
    ctx: SessionExtensionCtx,
}

impl SessionExtension for SpinnerDemoSession {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(LongTaskTool {
            ctx: self.ctx.clone(),
        })]
    }
}

struct LongTaskTool {
    ctx: SessionExtensionCtx,
}

#[async_trait]
impl Tool for LongTaskTool {
    fn name(&self) -> &str {
        "long_task"
    }

    fn description(&self) -> &str {
        "Run a short demo task that shows and clears a frontend status spinner."
    }

    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn call(
        &self,
        _params: Value,
        cancel: CancellationToken,
        _progress: Option<ProgressSink>,
    ) -> anyhow::Result<ToolResult> {
        self.ctx.emit_frontend_event(json!({
            "widget_id": WIDGET_ID,
            "kind": "spinner",
            "label": "running demo task"
        }))?;

        let result = tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(25)) => ToolResult::ok("Spinner demo task complete."),
            _ = cancel.cancelled() => ToolResult::err("Cancelled"),
        };

        self.ctx.emit_frontend_event(json!({
            "widget_id": WIDGET_ID,
            "kind": "clear"
        }))?;

        Ok(result)
    }
}

inventory::submit! {
    ExtensionRegistration {
        register: || Arc::new(SpinnerDemoExtension) as Arc<dyn DaemonCodeExtension>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn long_task_emits_spinner_then_clear_events() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let ctx = SessionExtensionCtx::new(
            std::path::PathBuf::from("/tmp/test"),
            "session".into(),
            Arc::new(vulcan::memory::SessionStore::in_memory()),
        )
        .for_extension(ID);
        let ctx = SessionExtensionCtx {
            frontend_events: vulcan::extensions::api::FrontendEventSink::new(tx),
            ..ctx
        };
        let session = SpinnerDemoExtension.instantiate(ctx);
        let tools = session.tools();
        let tool = tools
            .iter()
            .find(|tool| tool.name() == "long_task")
            .expect("long_task tool");

        let result = tool
            .call(json!({}), CancellationToken::new(), None)
            .await
            .expect("tool ok");

        assert!(!result.is_error);
        let first = rx.recv().await.expect("spinner event");
        let second = rx.recv().await.expect("clear event");
        assert_eq!(first.extension_id, ID);
        assert_eq!(first.payload["kind"], "spinner");
        assert_eq!(first.payload["widget_id"], WIDGET_ID);
        assert_eq!(second.payload["kind"], "clear");
        assert_eq!(second.payload["widget_id"], WIDGET_ID);
    }
}
