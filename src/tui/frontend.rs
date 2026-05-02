use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use vulcan_frontend_api::{
    FrontendCodeExtension, FrontendCommand, FrontendCommandAction, FrontendCtx, MessageRenderer,
    ToolResultView, WidgetUpdate,
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

    pub fn dispatch_slash(&self, input: &str) -> Option<FrontendCommandAction> {
        let body = input.strip_prefix('/')?.trim();
        let name = body.split_whitespace().next().unwrap_or("");
        let command = self.commands.get(name)?;
        let mut ctx = FrontendCtx::default();
        Some(command.run(&mut ctx))
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
