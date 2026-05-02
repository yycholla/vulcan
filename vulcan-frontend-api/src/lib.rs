//! Frontend extension registration API.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
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
        self.updates.drain(..).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum WidgetContent {
    Text(String),
    Spinner(String),
    Progress { label: String, ratio: f64 },
}

impl WidgetContent {
    pub fn progress(label: impl Into<String>, ratio: f64) -> Self {
        Self::Progress {
            label: label.into(),
            ratio: ratio.clamp(0.0, 1.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WidgetUpdate {
    pub id: String,
    pub content: Option<WidgetContent>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontendExtensionDescriptor {
    pub id: String,
    pub version: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    struct EventExtension;

    impl FrontendCodeExtension for EventExtension {
        fn id(&self) -> &'static str {
            "event"
        }

        fn on_event(&self, payload: &Value, ctx: &mut FrontendCtx) {
            ctx.ui.set_widget(
                "status",
                Some(WidgetContent::Text(
                    payload["message"].as_str().unwrap_or_default().to_string(),
                )),
            );
        }
    }

    #[test]
    fn extension_ui_records_set_and_clear_widget_updates() {
        let mut ui = ExtensionUi::default();

        ui.set_widget("job", Some(WidgetContent::Spinner("working".into())));
        ui.set_widget("job", None);

        assert_eq!(
            ui.drain_widget_updates(),
            vec![
                WidgetUpdate {
                    id: "job".into(),
                    content: Some(WidgetContent::Spinner("working".into())),
                },
                WidgetUpdate {
                    id: "job".into(),
                    content: None,
                },
            ]
        );
        assert!(ui.drain_widget_updates().is_empty());
    }

    #[test]
    fn progress_widget_ratio_is_clamped() {
        assert_eq!(
            WidgetContent::progress("done", 2.4),
            WidgetContent::Progress {
                label: "done".into(),
                ratio: 1.0
            }
        );
        assert_eq!(
            WidgetContent::progress("start", -1.0),
            WidgetContent::Progress {
                label: "start".into(),
                ratio: 0.0
            }
        );
    }

    #[test]
    fn frontend_extension_event_can_emit_widget_update() {
        let extension = EventExtension;
        let mut ctx = FrontendCtx::default().with_extension("event");

        extension.on_event(&serde_json::json!({ "message": "ready" }), &mut ctx);

        assert_eq!(
            ctx.ui.drain_widget_updates(),
            vec![WidgetUpdate {
                id: "status".into(),
                content: Some(WidgetContent::Text("ready".into())),
            }]
        );
    }
}
