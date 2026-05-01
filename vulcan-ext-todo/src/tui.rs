use std::sync::Arc;

use vulcan_frontend_api::{
    FrontendCodeExtension, FrontendCommand, FrontendCommandAction, FrontendCtx,
    FrontendExtensionRegistration, MessageRenderer, RenderedMessage, ToolResultView,
};

pub struct TodoFrontendExtension;

impl FrontendCodeExtension for TodoFrontendExtension {
    fn id(&self) -> &'static str {
        "todo"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn frontend_capabilities(&self) -> Vec<&'static str> {
        vec!["text_io", "rich_text"]
    }

    fn message_renderers(&self) -> Vec<Arc<dyn MessageRenderer>> {
        vec![Arc::new(TodoListRenderer)]
    }

    fn commands(&self) -> Vec<Arc<dyn FrontendCommand>> {
        vec![Arc::new(TodosCommand)]
    }
}

struct TodoListRenderer;

impl MessageRenderer for TodoListRenderer {
    fn tool_name(&self) -> &'static str {
        "todo_list"
    }

    fn render(&self, _ctx: &FrontendCtx, result: &ToolResultView<'_>) -> Option<RenderedMessage> {
        let items = result.details?.get("items")?.as_array()?;
        let mut lines = vec![format!("Todos ({})", items.len())];
        if items.is_empty() {
            lines.push("No todos yet.".to_string());
        } else {
            for item in items {
                lines.push(format!("[ ] {}", item.as_str().unwrap_or("<invalid todo>")));
            }
        }
        Some(RenderedMessage::from_lines(lines))
    }
}

struct TodosCommand;

impl FrontendCommand for TodosCommand {
    fn name(&self) -> &'static str {
        "todos"
    }

    fn description(&self) -> &'static str {
        "Open todo list"
    }

    fn run(&self, _ctx: &mut FrontendCtx) -> FrontendCommandAction {
        FrontendCommandAction::OpenView {
            id: "todo".into(),
            title: "Todos".into(),
            body: vec!["Todo view opened. Ask the agent to add or list todos.".into()],
        }
    }
}

inventory::submit! {
    FrontendExtensionRegistration {
        register: || Arc::new(TodoFrontendExtension) as Arc<dyn FrontendCodeExtension>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use vulcan_frontend_api::{
        FrontendCodeExtension, FrontendCommandAction, FrontendCtx, ToolResultView,
    };

    #[test]
    fn todo_frontend_extension_renders_todo_list_details() {
        let ext = TodoFrontendExtension;
        let renderers = ext.message_renderers();
        let renderer = renderers
            .iter()
            .find(|renderer| renderer.tool_name() == "todo_list")
            .expect("todo_list renderer");
        let details = json!({ "items": ["buy milk", "ship issue"] });
        let result = ToolResultView {
            tool_name: "todo_list",
            ok: true,
            output_preview: None,
            details: Some(&details),
        };

        let rendered = renderer
            .render(&FrontendCtx::default(), &result)
            .expect("rendered");

        assert!(rendered.lines.iter().any(|line| line.contains("Todos")));
        assert!(rendered.lines.iter().any(|line| line.contains("buy milk")));
        assert!(
            rendered
                .lines
                .iter()
                .any(|line| line.contains("ship issue"))
        );
    }

    #[test]
    fn todo_frontend_extension_declares_todos_command() {
        let ext = TodoFrontendExtension;
        let command = ext
            .commands()
            .into_iter()
            .find(|command| command.name() == "todos")
            .expect("/todos command");

        let action = command.run(&mut FrontendCtx::default());

        assert!(matches!(
            action,
            FrontendCommandAction::OpenView { ref id, .. } if id == "todo"
        ));
    }
}
