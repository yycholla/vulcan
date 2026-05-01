use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use vulcan_frontend_api::{
    FrontendCodeExtension, FrontendCommand, FrontendCommandAction, FrontendCtx, MessageRenderer,
    ToolResultView,
};

#[derive(Default)]
pub struct TuiFrontend {
    renderers: HashMap<&'static str, Arc<dyn MessageRenderer>>,
    commands: HashMap<&'static str, Arc<dyn FrontendCommand>>,
    capabilities: Vec<crate::extensions::FrontendCapability>,
}

impl TuiFrontend {
    pub fn collect() -> Self {
        Self::from_extensions(vulcan_frontend_api::collect_registrations())
    }

    pub fn from_extensions(extensions: Vec<Arc<dyn FrontendCodeExtension>>) -> Self {
        let mut this = Self::default();
        for extension in extensions {
            for cap in extension.frontend_capabilities() {
                if let Some(cap) = map_frontend_capability(cap)
                    && !this.capabilities.contains(&cap)
                {
                    this.capabilities.push(cap);
                }
            }
            for renderer in extension.message_renderers() {
                let tool_name = renderer.tool_name();
                if this.renderers.insert(tool_name, renderer).is_some() {
                    tracing::warn!(
                        extension_id = extension.id(),
                        tool_name,
                        "frontend renderer collision; last active renderer wins"
                    );
                }
            }
            for command in extension.commands() {
                this.commands.insert(command.name(), command);
            }
        }
        if !this
            .capabilities
            .contains(&crate::extensions::FrontendCapability::TextIo)
        {
            this.capabilities
                .push(crate::extensions::FrontendCapability::TextIo);
        }
        this
    }

    pub fn frontend_capabilities(&self) -> Vec<crate::extensions::FrontendCapability> {
        self.capabilities.clone()
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

    pub fn dispatch_slash(&self, input: &str) -> Option<FrontendCommandAction> {
        let body = input.strip_prefix('/')?.trim();
        let name = body.split_whitespace().next().unwrap_or("");
        let command = self.commands.get(name)?;
        let mut ctx = FrontendCtx::default();
        Some(command.run(&mut ctx))
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

fn map_frontend_capability(capability: &str) -> Option<crate::extensions::FrontendCapability> {
    match capability {
        "text_io" => Some(crate::extensions::FrontendCapability::TextIo),
        "rich_text" => Some(crate::extensions::FrontendCapability::RichText),
        "cell_canvas" => Some(crate::extensions::FrontendCapability::CellCanvas),
        "raw_input" => Some(crate::extensions::FrontendCapability::RawInput),
        "status_widgets" => Some(crate::extensions::FrontendCapability::StatusWidgets),
        other => {
            tracing::warn!(capability = other, "ignoring unknown frontend capability");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use serde_json::json;
    use vulcan_frontend_api::{
        FrontendCodeExtension, FrontendCommand, FrontendCommandAction, FrontendCtx,
        MessageRenderer, RenderedMessage, ToolResultView,
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

        fn frontend_capabilities(&self) -> Vec<&'static str> {
            vec!["text_io", "rich_text"]
        }

        fn message_renderers(&self) -> Vec<Arc<dyn MessageRenderer>> {
            vec![Arc::new(Renderer(self.renderer))]
        }

        fn commands(&self) -> Vec<Arc<dyn FrontendCommand>> {
            vec![Arc::new(Command)]
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

        let action = frontend.dispatch_slash("/todos").expect("local command");

        assert!(matches!(
            action,
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

        let caps = frontend.frontend_capabilities();

        assert!(caps.contains(&crate::extensions::FrontendCapability::TextIo));
        assert!(caps.contains(&crate::extensions::FrontendCapability::RichText));
    }
}
