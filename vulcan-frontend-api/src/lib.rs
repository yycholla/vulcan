//! Frontend extension registration API.

use std::sync::Arc;

use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct FrontendCtx {
    pub session_id: Option<String>,
    pub extension_id: Option<String>,
}

impl FrontendCtx {
    pub fn with_extension(mut self, extension_id: impl Into<String>) -> Self {
        self.extension_id = Some(extension_id.into());
        self
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
    fn frontend_capabilities(&self) -> Vec<&'static str> {
        Vec::new()
    }
    fn message_renderers(&self) -> Vec<Arc<dyn MessageRenderer>> {
        Vec::new()
    }
    fn commands(&self) -> Vec<Arc<dyn FrontendCommand>> {
        Vec::new()
    }
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
