//! Frontend extension registration API.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    surface_requests: Vec<FrontendSurface>,
    surface_updates: Vec<FrontendSurfaceUpdate>,
    surface_closes: Vec<String>,
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

    pub fn open_surface(&mut self, surface: FrontendSurface) {
        self.surface_requests.push(surface);
    }

    pub fn update_surface(&mut self, update: FrontendSurfaceUpdate) {
        self.surface_updates.push(update);
    }

    pub fn close_surface(&mut self, id: impl Into<String>) {
        self.surface_closes.push(id.into());
    }

    pub fn drain_widget_updates(&mut self) -> Vec<WidgetUpdate> {
        self.updates.drain(..).collect()
    }

    pub fn drain_canvas_requests(&mut self) -> Vec<CanvasRequest> {
        std::mem::take(&mut self.canvas_requests)
    }

    pub fn drain_tick_requests(&mut self) -> Vec<TickRequest> {
        std::mem::take(&mut self.tick_requests)
    }

    pub fn drain_surface_requests(&mut self) -> Vec<FrontendSurface> {
        std::mem::take(&mut self.surface_requests)
    }

    pub fn drain_surface_updates(&mut self) -> Vec<FrontendSurfaceUpdate> {
        std::mem::take(&mut self.surface_updates)
    }

    pub fn drain_surface_closes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.surface_closes)
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

    pub fn open_surface(&self, _surface: FrontendSurface) -> UiResult<()> {
        Err(UiError::NoUi)
    }

    pub fn update_surface(&self, _update: FrontendSurfaceUpdate) -> UiResult<()> {
        Err(UiError::NoUi)
    }

    pub fn close_surface(&self, _id: impl Into<String>) -> UiResult<()> {
        Err(UiError::NoUi)
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
pub enum FrontendSurfacePlacement {
    Modal,
    Fullscreen,
    RightDrawer,
    BottomDrawer,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrontendSurface {
    pub id: String,
    pub title: String,
    pub body: Vec<String>,
    pub placement: FrontendSurfacePlacement,
}

impl FrontendSurface {
    pub fn modal(id: impl Into<String>, title: impl Into<String>, body: Vec<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            body,
            placement: FrontendSurfacePlacement::Modal,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrontendSurfaceUpdate {
    pub id: String,
    pub title: Option<String>,
    pub body: Option<Vec<String>>,
    pub placement: Option<FrontendSurfacePlacement>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrontendCommandAction {
    Noop,
    SystemMessage(String),
    OpenSurface(FrontendSurface),
    UpdateSurface(FrontendSurfaceUpdate),
    CloseSurface {
        id: String,
    },
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

    struct TestCanvas;

    impl Canvas for TestCanvas {
        fn render(&self) -> CanvasFrame {
            CanvasFrame {
                title: "Test".into(),
                lines: vec!["ok".into()],
            }
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
    fn extension_ui_records_canvas_tick_and_surface_requests() {
        let mut ui = ExtensionUi::default();

        let canvas = ui
            .custom(CanvasFactory::new(|_handle| Box::new(TestCanvas)))
            .expect("canvas handle");
        let tick = ui
            .set_tick(TickRate::Tick30Hz, |_| {})
            .expect("tick handle");
        ui.open_surface(FrontendSurface::modal(
            "todos",
            "Todos",
            vec!["body".into()],
        ));
        ui.update_surface(FrontendSurfaceUpdate {
            id: "todos".into(),
            title: Some("Todos updated".into()),
            body: None,
            placement: None,
        });
        ui.close_surface("todos");

        assert!(!canvas.has_exited());
        assert!(!tick.is_stopped());
        assert_eq!(ui.drain_canvas_requests().len(), 1);
        assert_eq!(ui.drain_tick_requests().len(), 1);
        assert_eq!(
            ui.drain_surface_requests(),
            vec![FrontendSurface::modal(
                "todos",
                "Todos",
                vec!["body".into()]
            )]
        );
        assert_eq!(ui.drain_surface_updates().len(), 1);
        assert_eq!(ui.drain_surface_closes(), vec!["todos".to_string()]);
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
        assert_eq!(
            ui.open_surface(FrontendSurface::modal("x", "X", Vec::new())),
            Err(UiError::NoUi)
        );
        assert_eq!(
            ui.update_surface(FrontendSurfaceUpdate {
                id: "x".into(),
                ..FrontendSurfaceUpdate::default()
            }),
            Err(UiError::NoUi)
        );
        assert_eq!(ui.close_surface("x"), Err(UiError::NoUi));
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
