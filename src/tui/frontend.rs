use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use vulcan_frontend_api::{
    CanvasRequest, FrontendCodeExtension, FrontendCommand, FrontendCommandAction, FrontendCtx,
    MessageRenderer, TickRequest, ToolResultView, WidgetUpdate,
};

#[derive(Default)]
pub struct TuiFrontend {
    extensions: HashMap<&'static str, Arc<dyn FrontendCodeExtension>>,
    renderers: HashMap<&'static str, Arc<dyn MessageRenderer>>,
    commands: HashMap<&'static str, Arc<dyn FrontendCommand>>,
    capabilities: Vec<&'static str>,
}

impl TuiFrontend {
    pub fn collect() -> Self {
        Self::from_extensions(vulcan_frontend_api::collect_registrations())
    }

    pub fn from_extensions(extensions: Vec<Arc<dyn FrontendCodeExtension>>) -> Self {
        let mut this = Self::default();
        for extension in extensions {
            let extension_id = extension.id();
            for capability in extension.frontend_capabilities() {
                if !this.capabilities.contains(&capability) {
                    this.capabilities.push(capability);
                }
            }
            for renderer in extension.message_renderers() {
                let tool_name = renderer.tool_name();
                if this.renderers.insert(tool_name, renderer).is_some() {
                    tracing::warn!(
                        extension_id,
                        tool_name,
                        "frontend renderer collision; last active renderer wins"
                    );
                }
            }
            for command in extension.commands() {
                this.commands.insert(command.name(), command);
            }
            this.extensions.insert(extension_id, extension);
        }
        if !this.capabilities.contains(&"text_io") {
            this.capabilities.push("text_io");
        }
        this
    }

    pub fn frontend_capabilities(&self) -> Vec<&'static str> {
        self.capabilities.clone()
    }

    pub fn extension_frontend_capabilities(&self) -> Vec<crate::extensions::FrontendCapability> {
        let mut capabilities: Vec<_> = self
            .capabilities
            .iter()
            .filter_map(|capability| crate::extensions::FrontendCapability::parse(capability))
            .collect();
        if !capabilities.contains(&crate::extensions::FrontendCapability::TextIo) {
            capabilities.push(crate::extensions::FrontendCapability::TextIo);
        }
        capabilities
    }

    pub fn frontend_extensions(&self) -> Vec<vulcan_frontend_api::FrontendExtensionDescriptor> {
        let mut descriptors: Vec<_> = self
            .extensions
            .values()
            .map(
                |extension| vulcan_frontend_api::FrontendExtensionDescriptor {
                    id: extension.id().to_string(),
                    version: extension.version().to_string(),
                },
            )
            .collect();
        descriptors.sort_by(|a, b| a.id.cmp(&b.id));
        descriptors
    }

    pub fn render_tool_result(
        &self,
        tool_name: &str,
        ok: bool,
        output_preview: Option<&str>,
        details: Option<&Value>,
    ) -> Option<Vec<String>> {
        let renderer = self.renderers.get(tool_name)?;
        let ctx = FrontendCtx::default();
        let result = ToolResultView {
            tool_name,
            ok,
            output_preview,
            details,
        };
        renderer
            .render(&ctx, &result)
            .map(|rendered| rendered.lines)
    }

    pub fn dispatch_slash(&self, input: &str) -> Option<FrontendCommandDispatch> {
        let body = input.strip_prefix('/')?.trim();
        let name = body.split_whitespace().next().unwrap_or("");
        let command = self.commands.get(name)?;
        let mut ctx = FrontendCtx::default();
        let action = command.run(&mut ctx);
        Some(FrontendCommandDispatch {
            action,
            canvas_requests: ctx.ui.drain_canvas_requests(),
            tick_requests: ctx.ui.drain_tick_requests(),
            widget_updates: ctx.ui.drain_widget_updates(),
        })
    }

    pub fn handle_extension_event(&self, data: &Value) -> Vec<WidgetUpdate> {
        if data.get("kind").and_then(Value::as_str) != Some("extension_event") {
            tracing::warn!("frontend extension event missing extension_event kind");
            return Vec::new();
        }
        let Some(extension_id) = data.get("extension_id").and_then(Value::as_str) else {
            tracing::warn!("frontend extension event missing extension_id");
            return Vec::new();
        };
        let Some(extension) = self.extensions.get(extension_id) else {
            tracing::warn!(
                extension_id,
                "frontend extension event for unknown extension"
            );
            return Vec::new();
        };

        let mut ctx = FrontendCtx {
            session_id: data
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            extension_id: Some(extension_id.to_string()),
            ..FrontendCtx::default()
        };
        extension.on_event(data.get("payload").unwrap_or(&Value::Null), &mut ctx);
        ctx.ui.drain_widget_updates()
    }

    pub fn command_specs(&self) -> Vec<FrontendCommandSpec> {
        let mut specs: Vec<_> = self
            .commands
            .values()
            .map(|command| FrontendCommandSpec {
                name: command.name(),
                description: command.description(),
                mid_turn_safe: command.mid_turn_safe(),
            })
            .collect();
        specs.sort_by_key(|spec| spec.name);
        specs
    }

    pub fn is_frontend_command_mid_turn_safe(&self, name: &str) -> Option<bool> {
        self.commands
            .get(name)
            .map(|command| command.mid_turn_safe())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrontendCommandSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub mid_turn_safe: bool,
}

#[derive(Debug)]
pub struct FrontendCommandDispatch {
    pub action: FrontendCommandAction,
    pub canvas_requests: Vec<CanvasRequest>,
    pub tick_requests: Vec<TickRequest>,
    pub widget_updates: Vec<WidgetUpdate>,
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use serde_json::json;
    use vulcan_frontend_api::{
        FrontendCodeExtension, FrontendCommand, FrontendCommandAction, FrontendCtx,
        MessageRenderer, RenderedMessage, ToolResultView, WidgetContent,
    };

    struct Renderer(&'static str);

    impl MessageRenderer for Renderer {
        fn tool_name(&self) -> &'static str {
            "todo_list"
        }

        fn render(
            &self,
            _ctx: &FrontendCtx,
            _result: &ToolResultView<'_>,
        ) -> Option<RenderedMessage> {
            Some(RenderedMessage::from_lines([self.0]))
        }
    }

    struct Command;

    impl FrontendCommand for Command {
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
                body: vec!["body".into()],
            }
        }
    }

    struct Extension {
        id: &'static str,
        renderer: &'static str,
    }

    impl FrontendCodeExtension for Extension {
        fn id(&self) -> &'static str {
            self.id
        }

        fn version(&self) -> &'static str {
            "0.1.0"
        }

        fn frontend_capabilities(&self) -> Vec<&'static str> {
            vec!["text_io", "rich_text"]
        }

        fn message_renderers(&self) -> Vec<Arc<dyn MessageRenderer>> {
            vec![Arc::new(Renderer(self.renderer))]
        }

        fn commands(&self) -> Vec<Arc<dyn FrontendCommand>> {
            vec![Arc::new(Command)]
        }

        fn on_event(&self, payload: &Value, ctx: &mut FrontendCtx) {
            if let Some(label) = payload.get("spinner").and_then(Value::as_str) {
                ctx.ui
                    .set_widget("job", Some(WidgetContent::Spinner(label.to_string())));
            }
        }
    }

    #[test]
    fn renderer_collision_uses_last_extension() {
        let frontend = TuiFrontend::from_extensions(vec![
            Arc::new(Extension {
                id: "first",
                renderer: "first",
            }),
            Arc::new(Extension {
                id: "second",
                renderer: "second",
            }),
        ]);

        let details = json!({ "items": ["buy milk"] });
        let rendered =
            frontend.render_tool_result("todo_list", true, Some("1. buy milk"), Some(&details));

        assert_eq!(rendered, Some(vec!["second".into()]));
    }

    #[test]
    fn frontend_slash_command_dispatches_locally() {
        let frontend = TuiFrontend::from_extensions(vec![Arc::new(Extension {
            id: "todo",
            renderer: "todo",
        })]);

        let dispatch = frontend.dispatch_slash("/todos").expect("local command");

        assert!(matches!(
            dispatch.action,
            FrontendCommandAction::OpenView { ref id, .. } if id == "todo"
        ));
    }

    #[test]
    fn unknown_slash_command_falls_through() {
        let frontend = TuiFrontend::default();
        assert!(frontend.dispatch_slash("/does-not-exist").is_none());
    }

    #[test]
    fn maps_declared_capabilities_for_daemon_connection() {
        let frontend = TuiFrontend::from_extensions(vec![Arc::new(Extension {
            id: "todo",
            renderer: "todo",
        })]);

        let caps = frontend.extension_frontend_capabilities();

        assert!(caps.contains(&crate::extensions::FrontendCapability::TextIo));
        assert!(caps.contains(&crate::extensions::FrontendCapability::RichText));
    }

    #[test]
    fn frontend_event_dispatches_to_matching_extension_widget_updates() {
        let frontend = TuiFrontend::from_extensions(vec![Arc::new(Extension {
            id: "spinner",
            renderer: "todo",
        })]);

        let updates = frontend.handle_extension_event(&json!({
            "kind": "extension_event",
            "session_id": "main",
            "extension_id": "spinner",
            "payload": { "spinner": "working" }
        }));

        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].id, "job");
        assert_eq!(
            updates[0].content,
            Some(WidgetContent::Spinner("working".into()))
        );
    }

    #[test]
    fn frontend_event_for_unknown_extension_is_dropped() {
        let frontend = TuiFrontend::default();
        let updates = frontend.handle_extension_event(&json!({
            "kind": "extension_event",
            "extension_id": "missing",
            "payload": { "spinner": "working" }
        }));
        assert!(updates.is_empty());
    }
}
