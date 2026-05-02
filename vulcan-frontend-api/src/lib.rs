//! Frontend extension registration API.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiError {
    NoUi,
    Cancelled,
}

pub type UiResult<T> = Result<T, UiError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickRate {
    Tick30Hz,
    Tick60Hz,
}

impl TickRate {
    pub fn capability(self) -> &'static str {
        match self {
            Self::Tick30Hz => "tick_30hz",
            Self::Tick60Hz => "tick_60hz",
        }
    }

    pub fn millis(self) -> u64 {
        match self {
            Self::Tick30Hz => 33,
            Self::Tick60Hz => 16,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TickHandle {
    stopped: Arc<AtomicBool>,
}

impl TickHandle {
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Debug, Default)]
pub struct CanvasHandle {
    exited: Arc<AtomicBool>,
}

impl CanvasHandle {
    pub fn exit(&self) {
        self.exited.store(true, Ordering::SeqCst);
    }

    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanvasKey {
    Up,
    Down,
    Left,
    Right,
    Esc,
    CtrlC,
    Enter,
    Backspace,
    Char(char),
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanvasControl {
    Continue,
    Exit,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CanvasFrame {
    pub title: String,
    pub lines: Vec<String>,
}

pub trait Canvas: Send + Sync {
    fn render(&self) -> CanvasFrame;

    fn on_key(&self, key: CanvasKey, handle: &CanvasHandle) -> CanvasControl {
        match key {
            CanvasKey::Esc | CanvasKey::CtrlC => {
                handle.exit();
                CanvasControl::Exit
            }
            _ => CanvasControl::Continue,
        }
    }

    fn on_tick(&self, _handle: &CanvasHandle) {}
}

#[derive(Clone)]
pub struct CanvasFactory {
    factory: Arc<dyn Fn(CanvasHandle) -> Box<dyn Canvas> + Send + Sync>,
}

impl CanvasFactory {
    pub fn new<F>(factory: F) -> Self
    where
        F: Fn(CanvasHandle) -> Box<dyn Canvas> + Send + Sync + 'static,
    {
        Self {
            factory: Arc::new(factory),
        }
    }

    pub fn create(&self, handle: CanvasHandle) -> Box<dyn Canvas> {
        (self.factory)(handle)
    }
}

impl fmt::Debug for CanvasFactory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CanvasFactory").finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct CanvasRequest {
    pub handle: CanvasHandle,
    pub factory: CanvasFactory,
}

#[derive(Clone)]
pub struct TickRequest {
    pub rate: TickRate,
    pub handle: TickHandle,
    pub callback: Arc<dyn Fn(&TickHandle) + Send + Sync>,
}

impl fmt::Debug for TickRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TickRequest")
            .field("rate", &self.rate)
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ExtensionUi {
    updates: Vec<WidgetUpdate>,
    canvas_requests: Vec<CanvasRequest>,
    tick_requests: Vec<TickRequest>,
}

impl ExtensionUi {
    pub fn select(&mut self, _title: impl Into<String>, _options: &[String]) -> UiResult<usize> {
        Err(UiError::NoUi)
    }

    pub fn input(
        &mut self,
        _title: impl Into<String>,
        _placeholder: Option<&str>,
    ) -> UiResult<String> {
        Err(UiError::NoUi)
    }

    pub fn custom(&mut self, canvas_factory: CanvasFactory) -> UiResult<CanvasHandle> {
        let handle = CanvasHandle::default();
        self.canvas_requests.push(CanvasRequest {
            handle: handle.clone(),
            factory: canvas_factory,
        });
        Ok(handle)
    }

    pub fn set_tick<F>(&mut self, rate: TickRate, callback: F) -> UiResult<TickHandle>
    where
        F: Fn(&TickHandle) + Send + Sync + 'static,
    {
        let handle = TickHandle::default();
        self.tick_requests.push(TickRequest {
            rate,
            handle: handle.clone(),
            callback: Arc::new(callback),
        });
        Ok(handle)
    }

    pub fn set_widget(&mut self, id: impl Into<String>, content: Option<WidgetContent>) {
        self.updates.push(WidgetUpdate {
            id: id.into(),
            content,
        });
    }

    pub fn drain_widget_updates(&mut self) -> Vec<WidgetUpdate> {
        std::mem::take(&mut self.updates)
    }

    pub fn drain_canvas_requests(&mut self) -> Vec<CanvasRequest> {
        std::mem::take(&mut self.canvas_requests)
    }

    pub fn drain_tick_requests(&mut self) -> Vec<TickRequest> {
        std::mem::take(&mut self.tick_requests)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HeadlessUi;

impl HeadlessUi {
    pub fn select(&self, _title: impl Into<String>, _options: &[String]) -> UiResult<usize> {
        Err(UiError::NoUi)
    }

    pub fn input(&self, _title: impl Into<String>, _placeholder: Option<&str>) -> UiResult<String> {
        Err(UiError::NoUi)
    }

    pub fn custom(&self, _canvas_factory: CanvasFactory) -> UiResult<CanvasHandle> {
        Err(UiError::NoUi)
    }

    pub fn set_tick<F>(&self, _rate: TickRate, _callback: F) -> UiResult<TickHandle>
    where
        F: Fn(&TickHandle) + Send + Sync + 'static,
    {
        Err(UiError::NoUi)
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

    struct TestCanvas;

    impl Canvas for TestCanvas {
        fn render(&self) -> CanvasFrame {
            CanvasFrame {
                title: "Test".into(),
                lines: vec!["ok".into()],
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
    fn extension_ui_records_canvas_and_tick_requests() {
        let mut ui = ExtensionUi::default();

        let canvas = ui
            .custom(CanvasFactory::new(|_handle| Box::new(TestCanvas)))
            .expect("canvas handle");
        let tick = ui
            .set_tick(TickRate::Tick30Hz, |_| {})
            .expect("tick handle");

        assert!(!canvas.has_exited());
        assert!(!tick.is_stopped());
        assert_eq!(ui.drain_canvas_requests().len(), 1);
        assert_eq!(ui.drain_tick_requests().len(), 1);
    }

    #[test]
    fn headless_ui_rejects_interactive_surfaces() {
        let ui = HeadlessUi;
        let options = vec!["one".to_string()];

        assert_eq!(ui.select("Pick", &options), Err(UiError::NoUi));
        assert_eq!(ui.input("Name", Some("Ada")), Err(UiError::NoUi));
        assert_eq!(
            ui.custom(CanvasFactory::new(|_handle| Box::new(TestCanvas)))
                .map(|_| ()),
            Err(UiError::NoUi)
        );
        assert_eq!(
            ui.set_tick(TickRate::Tick60Hz, |_| {}).map(|_| ()),
            Err(UiError::NoUi)
        );
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
