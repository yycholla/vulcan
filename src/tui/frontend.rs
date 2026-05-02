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
    capabilities: Vec<&'static str>,
}

impl TuiFrontend {
    pub fn collect() -> Self {
        Self::from_extensions(vulcan_frontend_api::collect_registrations())
    }

    pub fn from_extensions(extensions: Vec<Arc<dyn FrontendCodeExtension>>) -> Self {
        let mut this = Self::default();
        for extension in extensions {
            for capability in extension.frontend_capabilities() {
                if !this.capabilities.contains(&capability) {
                    this.capabilities.push(capability);
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
        if !this.capabilities.contains(&"text_io") {
            this.capabilities.push("text_io");
        }
        this
    }

    pub fn frontend_capabilities(&self) -> Vec<&'static str> {
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
