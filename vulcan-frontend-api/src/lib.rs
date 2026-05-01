//! Frontend extension registration API.

use std::sync::Arc;

use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct FrontendCtx {
    pub session_id: Option<String>,
    pub extension_id: Option<String>,
    pub ui: ExtensionUi,
}

impl FrontendCtx {
    pub fn with_extension(mut self, extension_id: impl Into<String>) -> Self {
        self.extension_id = Some(extension_id.into());
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum WidgetContent {
    Text(String),
    Spinner { label: String },
    Progress { label: String, ratio: f32 },
}

impl WidgetContent {
    pub fn progress(label: impl Into<String>, ratio: f32) -> Self {
        Self::Progress {
            label: label.into(),
            ratio: ratio.clamp(0.0, 1.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WidgetUpdate {
    pub id: String,
    pub content: Option<WidgetContent>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExtensionUi {
    updates: Vec<WidgetUpdate>,
}

impl ExtensionUi {
    pub fn set_widget(&mut self, id: impl Into<String>, content: Option<WidgetContent>) {
        self.updates.push(WidgetUpdate {
            id: id.into(),
            content,
        });
    }

    pub fn drain_widget_updates(&mut self) -> Vec<WidgetUpdate> {
        std::mem::take(&mut self.updates)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedMessage {
    pub lines: Vec<String>,
}

impl RenderedMessage {
    pub fn from_lines<I, S>(lines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            lines: lines.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ToolResultView<'a> {
    pub tool_name: &'a str,
    pub ok: bool,
    pub output_preview: Option<&'a str>,
    pub details: Option<&'a Value>,
}

pub trait MessageRenderer: Send + Sync {
    fn tool_name(&self) -> &'static str;
    fn render(&self, ctx: &FrontendCtx, result: &ToolResultView<'_>) -> Option<RenderedMessage>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrontendCommandAction {
    Noop,
    SystemMessage(String),
    OpenView {
        id: String,
        title: String,
        body: Vec<String>,
    },
}

pub trait FrontendCommand: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn mid_turn_safe(&self) -> bool {
        true
    }
    fn run(&self, ctx: &mut FrontendCtx) -> FrontendCommandAction;
}

pub trait FrontendCodeExtension: Send + Sync {
    fn id(&self) -> &'static str;
    fn version(&self) -> &'static str {
        "0.0.0"
    }
    fn frontend_capabilities(&self) -> Vec<&'static str> {
        Vec::new()
    }
    fn message_renderers(&self) -> Vec<Arc<dyn MessageRenderer>> {
        Vec::new()
    }
    fn commands(&self) -> Vec<Arc<dyn FrontendCommand>> {
        Vec::new()
    }
    fn on_event(&self, _payload: &Value, _ctx: &mut FrontendCtx) {}
}

pub struct FrontendExtensionRegistration {
    pub register: fn() -> Arc<dyn FrontendCodeExtension>,
}

inventory::collect!(FrontendExtensionRegistration);

pub fn collect_registrations() -> Vec<Arc<dyn FrontendCodeExtension>> {
    inventory::iter::<FrontendExtensionRegistration>
        .into_iter()
        .map(|entry| (entry.register)())
        .collect()
}

pub fn collect_frontend_capabilities() -> Vec<&'static str> {
    let mut caps = Vec::new();
    for extension in collect_registrations() {
        for cap in extension.frontend_capabilities() {
            if !caps.contains(&cap) {
                caps.push(cap);
            }
        }
    }
    caps
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FrontendExtensionDescriptor {
    pub id: String,
    pub version: String,
}

pub fn collect_frontend_descriptors() -> Vec<FrontendExtensionDescriptor> {
    collect_registrations()
        .into_iter()
        .map(|extension| FrontendExtensionDescriptor {
            id: extension.id().to_string(),
            version: extension.version().to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;

    struct TestRenderer;

    impl MessageRenderer for TestRenderer {
        fn tool_name(&self) -> &'static str {
            "todo_list"
        }

        fn render(
            &self,
            _ctx: &FrontendCtx,
            result: &ToolResultView<'_>,
        ) -> Option<RenderedMessage> {
            let count = result.details?.get("items")?.as_array()?.len();
            Some(RenderedMessage::from_lines([format!("todos: {count}")]))
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
                body: vec!["No todos yet.".into()],
            }
        }
    }

    struct TestExtension;

    impl FrontendCodeExtension for TestExtension {
        fn id(&self) -> &'static str {
            "test"
        }

        fn version(&self) -> &'static str {
            "1.2.3"
        }

        fn frontend_capabilities(&self) -> Vec<&'static str> {
            vec!["text_io", "rich_text"]
        }

        fn message_renderers(&self) -> Vec<Arc<dyn MessageRenderer>> {
            vec![Arc::new(TestRenderer)]
        }

        fn commands(&self) -> Vec<Arc<dyn FrontendCommand>> {
            vec![Arc::new(TestCommand)]
        }

        fn on_event(&self, payload: &Value, ctx: &mut FrontendCtx) {
            if payload.get("spin").and_then(Value::as_bool) == Some(true) {
                ctx.ui.set_widget(
                    "test",
                    Some(WidgetContent::Spinner {
                        label: "working".into(),
                    }),
                );
            }
        }
    }

    inventory::submit! {
        FrontendExtensionRegistration {
            register: || Arc::new(TestExtension) as Arc<dyn FrontendCodeExtension>,
        }
    }

    #[test]
    fn inventory_collects_frontend_extensions() {
        let extensions = collect_registrations();
        assert!(extensions.iter().any(|ext| ext.id() == "test"));
    }

    #[test]
    fn renderer_receives_tool_details_payload() {
        let renderer = TestRenderer;
        let ctx = FrontendCtx::default();
        let details = json!({ "items": ["buy milk", "ship issue"] });
        let result = ToolResultView {
            tool_name: "todo_list",
            ok: true,
            output_preview: Some("1. buy milk"),
            details: Some(&details),
        };

        let rendered = renderer.render(&ctx, &result).expect("rendered");
        assert_eq!(rendered.lines, vec!["todos: 2"]);
    }

    #[test]
    fn frontend_command_returns_local_action() {
        let mut ctx = FrontendCtx::default();
        let action = TestCommand.run(&mut ctx);
        assert!(matches!(
            action,
            FrontendCommandAction::OpenView { ref id, .. } if id == "todo"
        ));
    }

    #[test]
    fn extension_ui_records_widget_updates() {
        let mut ui = ExtensionUi::default();
        ui.set_widget("job", Some(WidgetContent::Text("ready".into())));
        ui.set_widget("job", None);

        let updates = ui.drain_widget_updates();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].id, "job");
        assert_eq!(
            updates[0].content,
            Some(WidgetContent::Text("ready".into()))
        );
        assert_eq!(updates[1].content, None);
        assert!(ui.drain_widget_updates().is_empty());
    }

    #[test]
    fn frontend_extension_handles_events_via_ctx_ui() {
        let ext = TestExtension;
        let mut ctx = FrontendCtx::default().with_extension(ext.id());
        ext.on_event(&json!({ "spin": true }), &mut ctx);

        let updates = ctx.ui.drain_widget_updates();
        assert_eq!(
            updates[0].content,
            Some(WidgetContent::Spinner {
                label: "working".into()
            })
        );
    }
}
