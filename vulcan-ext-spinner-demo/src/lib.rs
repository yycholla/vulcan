use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use vulcan::extensions::api::{
    DaemonCodeExtension, ExtensionContext, ExtensionRegistration, SessionExtension,
    SessionExtensionCtx,
};
use vulcan::extensions::{
    ExtensionCapability, ExtensionMetadata, ExtensionSource, ExtensionStatus, FrontendCapability,
};
use vulcan::tools::{ProgressSink, ReplaySafety, Tool, ToolResult};

const ID: &str = "spinner-demo";

#[cfg(feature = "tui")]
pub mod tui;

#[derive(Default)]
pub struct SpinnerDemoExtension;

impl DaemonCodeExtension for SpinnerDemoExtension {
    fn metadata(&self) -> ExtensionMetadata {
        let mut m = ExtensionMetadata::new(
            ID,
            "Spinner Demo",
            env!("CARGO_PKG_VERSION"),
            ExtensionSource::Builtin,
        );
        m.status = ExtensionStatus::Active;
        m.capabilities = vec![ExtensionCapability::ToolProvider];
        m.requires = vec![FrontendCapability::StatusWidgets];
        m.description = "Demo extension that drives a frontend status widget.".to_string();
        m
    }

    fn instantiate(&self, ctx: SessionExtensionCtx) -> Arc<dyn SessionExtension> {
        Arc::new(SpinnerDemoSession { ctx })
    }
}

struct SpinnerDemoSession {
    ctx: ExtensionContext,
}

impl SessionExtension for SpinnerDemoSession {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(LongTaskTool {
            ctx: self.ctx.clone(),
        })]
    }
}

struct LongTaskTool {
    ctx: ExtensionContext,
}

#[async_trait]
impl Tool for LongTaskTool {
    fn name(&self) -> &str {
        "long_task"
    }

    fn description(&self) -> &str {
        "Run a short demo task that shows a spinner status widget."
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
            "widget_id": "long_task",
            "kind": "spinner",
            "label": "running demo task",
        }))?;
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {}
            _ = cancel.cancelled() => {
                self.ctx.emit_frontend_event(json!({
                    "widget_id": "long_task",
                    "kind": "clear",
                }))?;
                return Ok(ToolResult::err("spinner demo task cancelled"));
            }
        }
        self.ctx.emit_frontend_event(json!({
            "widget_id": "long_task",
            "kind": "clear",
        }))?;
        Ok(ToolResult::ok("Spinner demo task complete."))
    }

    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::ReadOnly
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

    fn ctx() -> (
        SessionExtensionCtx,
        tokio::sync::broadcast::Receiver<vulcan::extensions::api::FrontendEvent>,
    ) {
        let (tx, rx) = tokio::sync::broadcast::channel(8);
        (
            SessionExtensionCtx {
                cwd: std::path::PathBuf::from("/tmp/spinner"),
                session_id: "spinner-session".into(),
                memory: Arc::new(vulcan::memory::SessionStore::in_memory()),
                frontend_capabilities: FrontendCapability::full_set(),
                frontend_extensions: Vec::new(),
                state: vulcan::extensions::ExtensionStateContext::in_memory_for_tests(
                    "spinner-session",
                    ID,
                ),
                frontend_events: vulcan::extensions::api::FrontendEventSink::new(tx),
            },
            rx,
        )
    }

    #[tokio::test]
    async fn long_task_emits_spinner_then_clear_events() {
        let (ctx, mut rx) = ctx();
        let session = SpinnerDemoExtension.instantiate(ctx);
        let tool = session.tools().remove(0);

        let result = tool
            .call(json!({}), CancellationToken::new(), None)
            .await
            .expect("tool result");

        assert!(result.output.contains("complete"));
        let start = rx.try_recv().expect("start event");
        let clear = rx.try_recv().expect("clear event");
        assert_eq!(start.extension_id, ID);
        assert_eq!(start.payload["kind"], "spinner");
        assert_eq!(clear.payload["kind"], "clear");
    }
}
