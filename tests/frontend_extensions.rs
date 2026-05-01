use std::sync::Arc;

use serde_json::json;
use vulcan::tools::ToolResult;
use vulcan::tui::frontend::TuiFrontend;
use vulcan_frontend_api::{
    FrontendCodeExtension, FrontendCommand, FrontendCommandAction, FrontendCtx, MessageRenderer,
    RenderedMessage, ToolResultView, WidgetContent,
};

struct TestRenderer(&'static str);

impl MessageRenderer for TestRenderer {
    fn tool_name(&self) -> &'static str {
        "todo_list"
    }

    fn render(&self, _ctx: &FrontendCtx, result: &ToolResultView<'_>) -> Option<RenderedMessage> {
        let items = result.details?.get("items")?.as_array()?;
        Some(RenderedMessage::from_lines([
            self.0.to_string(),
            format!("{} item(s)", items.len()),
        ]))
    }
}

struct TestCommand;

impl FrontendCommand for TestCommand {
    fn name(&self) -> &'static str {
        "todos"
    }

    fn description(&self) -> &'static str {
        "Open todos"
    }

    fn run(&self, _ctx: &mut FrontendCtx) -> FrontendCommandAction {
        FrontendCommandAction::OpenView {
            id: "todo".into(),
            title: "Todos".into(),
            body: vec!["Todo view opened.".into()],
        }
    }
}

struct TestExtension {
    id: &'static str,
    renderer_label: &'static str,
}

impl FrontendCodeExtension for TestExtension {
    fn id(&self) -> &'static str {
        self.id
    }

    fn frontend_capabilities(&self) -> Vec<&'static str> {
        vec!["text_io", "rich_text"]
    }

    fn message_renderers(&self) -> Vec<Arc<dyn MessageRenderer>> {
        vec![Arc::new(TestRenderer(self.renderer_label))]
    }

    fn commands(&self) -> Vec<Arc<dyn FrontendCommand>> {
        vec![Arc::new(TestCommand)]
    }

    fn version(&self) -> &'static str {
        "1.2.3"
    }

    fn on_event(&self, payload: &serde_json::Value, ctx: &mut FrontendCtx) {
        ctx.ui.set_widget(
            "status",
            Some(WidgetContent::Text(
                payload["message"].as_str().unwrap_or_default().to_string(),
            )),
        );
    }
}

#[test]
fn tui_frontend_uses_last_renderer_and_dispatches_frontend_commands() {
    let frontend = TuiFrontend::from_extensions(vec![
        Arc::new(TestExtension {
            id: "first",
            renderer_label: "first",
        }),
        Arc::new(TestExtension {
            id: "second",
            renderer_label: "second",
        }),
    ]);
    let details = json!({ "items": ["buy milk", "ship issue"] });

    let rendered = frontend
        .render_tool_result("todo_list", true, Some("fallback"), Some(&details))
        .expect("todo renderer should handle details");
    let action = frontend
        .dispatch_slash("/todos")
        .expect("frontend command should dispatch locally");

    assert_eq!(
        rendered,
        vec!["second".to_string(), "2 item(s)".to_string()]
    );
    assert!(matches!(
        action.action,
        FrontendCommandAction::OpenView { ref id, .. } if id == "todo"
    ));
}

#[test]
fn tool_result_details_are_available_to_frontend_renderers() {
    let result = ToolResult::ok("listed todos").with_details(json!({
        "items": ["buy milk"]
    }));
    let frontend = TuiFrontend::from_extensions(vec![Arc::new(TestExtension {
        id: "todo",
        renderer_label: "todo",
    })]);

    let rendered = frontend
        .render_tool_result(
            "todo_list",
            !result.is_error,
            Some(&result.output),
            result.details.as_ref(),
        )
        .expect("renderer should receive tool result details");

    assert_eq!(rendered, vec!["todo".to_string(), "1 item(s)".to_string()]);
}

#[test]
fn frontend_event_dispatches_to_matching_extension_widget_updates() {
    let frontend = TuiFrontend::from_extensions(vec![Arc::new(TestExtension {
        id: "todo",
        renderer_label: "todo",
    })]);

    let updates = frontend.handle_extension_event(&json!({
        "kind": "extension_event",
        "extension_id": "todo",
        "payload": { "message": "synced" }
    }));

    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].id, "status");
    assert_eq!(
        updates[0].content,
        Some(WidgetContent::Text("synced".into()))
    );
}

#[test]
fn unknown_frontend_extension_event_is_dropped() {
    let frontend = TuiFrontend::from_extensions(vec![Arc::new(TestExtension {
        id: "todo",
        renderer_label: "todo",
    })]);

    let updates = frontend.handle_extension_event(&json!({
        "kind": "extension_event",
        "extension_id": "missing",
        "payload": { "message": "ignored" }
    }));

    assert!(updates.is_empty());
}

#[test]
fn frontend_reports_extension_descriptors() {
    let frontend = TuiFrontend::from_extensions(vec![Arc::new(TestExtension {
        id: "todo",
        renderer_label: "todo",
    })]);

    assert_eq!(frontend.frontend_extensions().len(), 1);
    assert_eq!(frontend.frontend_extensions()[0].id, "todo");
    assert_eq!(frontend.frontend_extensions()[0].version, "1.2.3");
}
