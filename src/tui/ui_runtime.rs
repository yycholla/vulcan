//! Retained TUI runtime for transient surfaces.
//!
//! This is Vulcan's domain-specific layer over Ratatui's immediate-mode
//! renderer. It borrows the useful framework ideas from component TUIs:
//! mounted IDs, focus restoration, and explicit surface lifecycle, while
//! keeping placement, streaming cadence, and extension ABI vocabulary owned by
//! Vulcan.

use super::diff_scrubber::{DiffScrubberAction, DiffScrubberOutcome, DiffScrubberState};
use super::focus::FocusStack;
use super::input::TuiKeyEvent;
use super::model_picker::{ModelPickerOutcome, ModelPickerState};
use super::pause_prompt::{PausePromptOutcome, PausePromptState};
use super::picker_state::ProviderPickerEntry;
use super::provider_picker::{ProviderPickerOutcome, ProviderPickerState};
use super::surface::{
    FocusPolicy, SurfaceClosePolicy, SurfaceFrame, SurfaceId, SurfaceKind, SurfacePlacement,
    SurfaceSpec, UiEffect,
};

#[derive(Default)]
pub struct UiRuntime {
    surfaces: Vec<MountedSurface>,
    focus: FocusStack,
    next_surface: u64,
}

struct MountedSurface {
    spec: SurfaceSpec,
    layer: SurfaceLayer,
}

enum SurfaceLayer {
    Canvas(CanvasSurface),
    DiffScrubber(DiffScrubberState),
    PausePrompt(PausePromptState),
    Text(TextSurface),
    ModelPicker(ModelPickerState),
    SessionPicker(SessionPickerSurface),
    ProviderPicker(ProviderPickerState),
}

struct CanvasSurface {
    handle: vulcan_frontend_api::CanvasHandle,
    canvas: Box<dyn vulcan_frontend_api::Canvas>,
}

struct SessionPickerSurface {
    selection: usize,
}

struct TextSurface {
    title: String,
    body: Vec<String>,
}

impl UiRuntime {
    pub fn mount_canvas(&mut self, request: vulcan_frontend_api::CanvasRequest) {
        let id = self.next_id("canvas");
        let canvas = request.factory.create(request.handle.clone());
        let spec = SurfaceSpec::new(
            id.as_str(),
            "Canvas",
            SurfaceKind::Fullscreen,
            SurfacePlacement::Fullscreen,
        )
        .with_close_policy(SurfaceClosePolicy {
            esc_closes: true,
            ctrl_c_closes: true,
            modal_blocks: false,
            deny_on_close: false,
        });
        self.mount(MountedSurface {
            spec,
            layer: SurfaceLayer::Canvas(CanvasSurface {
                handle: request.handle,
                canvas,
            }),
        });
    }

    pub fn open_provider_picker(&mut self, items: Vec<ProviderPickerEntry>, selection: usize) {
        let spec = SurfaceSpec::new(
            "provider-picker",
            "Providers",
            SurfaceKind::Modal,
            SurfacePlacement::Modal {
                width: 96,
                height: 28,
            },
        )
        .with_close_policy(SurfaceClosePolicy::modal());
        self.mount(MountedSurface {
            spec,
            layer: SurfaceLayer::ProviderPicker(ProviderPickerState::new(items, selection)),
        });
    }

    pub fn open_model_picker(&mut self, state: ModelPickerState) {
        let spec = SurfaceSpec::new(
            "model-picker",
            "Models",
            SurfaceKind::Modal,
            SurfacePlacement::Fullscreen,
        )
        .with_close_policy(SurfaceClosePolicy::modal());
        self.mount(MountedSurface {
            spec,
            layer: SurfaceLayer::ModelPicker(state),
        });
    }

    pub fn open_diff_scrubber(
        &mut self,
        path: String,
        hunks: Vec<crate::pause::DiffScrubHunk>,
        pause: crate::pause::AgentPause,
    ) {
        let spec = SurfaceSpec::new(
            "diff-scrubber",
            "Diff Scrubber",
            SurfaceKind::Modal,
            SurfacePlacement::Modal {
                width: 88,
                height: 28,
            },
        )
        .with_close_policy(SurfaceClosePolicy::approval());
        self.mount(MountedSurface {
            spec,
            layer: SurfaceLayer::DiffScrubber(DiffScrubberState::new(path, hunks, pause)),
        });
    }

    pub fn open_pause_prompt(&mut self, summary: String, pause: crate::pause::AgentPause) {
        let spec = SurfaceSpec::new(
            "pause-prompt",
            "Pause",
            SurfaceKind::Modal,
            SurfacePlacement::Modal {
                width: 88,
                height: 12,
            },
        )
        .with_close_policy(SurfaceClosePolicy::approval());
        self.mount(MountedSurface {
            spec,
            layer: SurfaceLayer::PausePrompt(PausePromptState::new(summary, pause)),
        });
    }

    pub fn open_text_surface(&mut self, surface: vulcan_frontend_api::FrontendSurface) {
        let placement = frontend_placement_to_surface_placement(surface.placement);
        let spec = SurfaceSpec::new(
            surface.id,
            surface.title.clone(),
            SurfaceKind::Modal,
            placement,
        )
        .with_close_policy(SurfaceClosePolicy::modal());
        self.mount(MountedSurface {
            spec,
            layer: SurfaceLayer::Text(TextSurface {
                title: surface.title,
                body: surface.body,
            }),
        });
    }

    pub fn open_session_picker(&mut self, selection: usize) {
        let spec = SurfaceSpec::new(
            "session-picker",
            "Sessions",
            SurfaceKind::Modal,
            SurfacePlacement::Modal {
                width: 80,
                height: 24,
            },
        )
        .with_close_policy(SurfaceClosePolicy::modal());
        self.mount(MountedSurface {
            spec,
            layer: SurfaceLayer::SessionPicker(SessionPickerSurface { selection }),
        });
    }

    pub fn has_canvas(&self) -> bool {
        self.surfaces
            .iter()
            .any(|surface| matches!(surface.layer, SurfaceLayer::Canvas(_)))
    }

    pub fn has_provider_picker(&self) -> bool {
        self.surfaces
            .iter()
            .any(|surface| matches!(surface.layer, SurfaceLayer::ProviderPicker(_)))
    }

    pub fn has_model_picker(&self) -> bool {
        self.surfaces
            .iter()
            .any(|surface| matches!(surface.layer, SurfaceLayer::ModelPicker(_)))
    }

    pub fn has_diff_scrubber(&self) -> bool {
        self.surfaces
            .iter()
            .any(|surface| matches!(surface.layer, SurfaceLayer::DiffScrubber(_)))
    }

    pub fn has_pause_prompt(&self) -> bool {
        self.surfaces
            .iter()
            .any(|surface| matches!(surface.layer, SurfaceLayer::PausePrompt(_)))
    }

    pub fn has_text_surface(&self) -> bool {
        self.surfaces
            .iter()
            .any(|surface| matches!(surface.layer, SurfaceLayer::Text(_)))
    }

    pub fn has_modal_blocking_surface(&self) -> bool {
        self.surfaces
            .iter()
            .any(|surface| surface.spec.close_policy.modal_blocks)
    }

    pub fn surface_count(&self) -> usize {
        self.surfaces.len()
    }

    pub fn active_surface_title(&self) -> Option<&str> {
        self.focus
            .active()
            .and_then(|id| self.surface(id))
            .or_else(|| self.surfaces.last())
            .map(|surface| surface.spec.title.as_str())
    }

    pub fn close_text_surface(&mut self) -> bool {
        let Some(idx) = self
            .surfaces
            .iter()
            .rposition(|surface| matches!(surface.layer, SurfaceLayer::Text(_)))
        else {
            return false;
        };
        self.close_index(idx);
        true
    }

    pub fn close_surface(&mut self, id: impl Into<SurfaceId>) -> bool {
        let id = id.into();
        let Some(idx) = self.surface_index(&id) else {
            return false;
        };
        self.close_index(idx);
        true
    }

    pub fn update_text_surface(
        &mut self,
        update: vulcan_frontend_api::FrontendSurfaceUpdate,
    ) -> bool {
        let id = SurfaceId::new(update.id);
        let Some(idx) = self.surface_index(&id) else {
            return false;
        };
        let surface = &mut self.surfaces[idx];
        if let Some(placement) = update.placement {
            surface.spec.placement = frontend_placement_to_surface_placement(placement);
        }
        match &mut surface.layer {
            SurfaceLayer::Text(text) => {
                if let Some(title) = update.title {
                    surface.spec.title = title.clone();
                    text.title = title;
                }
                if let Some(body) = update.body {
                    text.body = body;
                }
                true
            }
            _ => false,
        }
    }

    pub fn pause_prompt_state(&self) -> Option<&PausePromptState> {
        self.surfaces
            .iter()
            .rev()
            .find_map(|surface| match &surface.layer {
                SurfaceLayer::PausePrompt(state) => Some(state),
                _ => None,
            })
    }

    pub fn handle_pause_prompt_key(&mut self, key: TuiKeyEvent) -> PausePromptOutcome {
        let Some(idx) = self
            .surfaces
            .iter()
            .rposition(|surface| matches!(surface.layer, SurfaceLayer::PausePrompt(_)))
        else {
            return PausePromptOutcome {
                pause: None,
                resume: None,
                label: None,
            };
        };
        let outcome = match &mut self.surfaces[idx].layer {
            SurfaceLayer::PausePrompt(state) => state.handle_key(key),
            _ => PausePromptOutcome {
                pause: None,
                resume: None,
                label: None,
            },
        };
        if outcome.resume.is_some() {
            self.close_index(idx);
        }
        outcome
    }

    pub fn close_pause_prompt(&mut self) -> Option<crate::pause::AgentPause> {
        let idx = self
            .surfaces
            .iter()
            .rposition(|surface| matches!(surface.layer, SurfaceLayer::PausePrompt(_)))?;
        let pause = match &mut self.surfaces[idx].layer {
            SurfaceLayer::PausePrompt(state) => state.take_pause(),
            _ => None,
        };
        self.close_index(idx);
        pause
    }

    pub fn diff_scrubber_state(&self) -> Option<&DiffScrubberState> {
        self.surfaces
            .iter()
            .rev()
            .find_map(|surface| match &surface.layer {
                SurfaceLayer::DiffScrubber(state) => Some(state),
                _ => None,
            })
    }

    pub fn handle_diff_scrubber_key(&mut self, key: TuiKeyEvent) -> DiffScrubberOutcome {
        let Some(idx) = self
            .surfaces
            .iter()
            .rposition(|surface| matches!(surface.layer, SurfaceLayer::DiffScrubber(_)))
        else {
            return DiffScrubberOutcome {
                pause: None,
                action: DiffScrubberAction::Cancel,
                total: 0,
            };
        };
        let (action, pause, total) = match &mut self.surfaces[idx].layer {
            SurfaceLayer::DiffScrubber(state) => {
                let action = state.handle_key(key);
                let total = state.hunks.len();
                let pause = if matches!(
                    action,
                    DiffScrubberAction::Accept(_) | DiffScrubberAction::Cancel
                ) {
                    state.take_pause()
                } else {
                    None
                };
                (action, pause, total)
            }
            _ => (DiffScrubberAction::Continue, None, 0),
        };
        if matches!(
            action,
            DiffScrubberAction::Accept(_) | DiffScrubberAction::Cancel
        ) {
            self.close_index(idx);
        }
        DiffScrubberOutcome {
            pause,
            action,
            total,
        }
    }

    pub fn close_diff_scrubber(&mut self) -> Option<crate::pause::AgentPause> {
        let idx = self
            .surfaces
            .iter()
            .rposition(|surface| matches!(surface.layer, SurfaceLayer::DiffScrubber(_)))?;
        let pause = match &mut self.surfaces[idx].layer {
            SurfaceLayer::DiffScrubber(state) => state.take_pause(),
            _ => None,
        };
        self.close_index(idx);
        pause
    }

    pub fn has_session_picker(&self) -> bool {
        self.surfaces
            .iter()
            .any(|surface| matches!(surface.layer, SurfaceLayer::SessionPicker(_)))
    }

    pub fn session_picker_selection(&self) -> usize {
        self.surfaces
            .iter()
            .rev()
            .find_map(|surface| match &surface.layer {
                SurfaceLayer::SessionPicker(picker) => Some(picker.selection),
                _ => None,
            })
            .unwrap_or(0)
    }

    pub fn session_picker_up(&mut self) {
        if let Some(SurfaceLayer::SessionPicker(picker)) = self.active_layer_mut() {
            picker.selection = picker.selection.saturating_sub(1);
        }
    }

    pub fn session_picker_down(&mut self, max: usize) {
        if let Some(SurfaceLayer::SessionPicker(picker)) = self.active_layer_mut() {
            picker.selection = picker.selection.saturating_add(1).min(max);
        }
    }

    pub fn close_session_picker(&mut self) -> bool {
        let Some(idx) = self
            .surfaces
            .iter()
            .rposition(|surface| matches!(surface.layer, SurfaceLayer::SessionPicker(_)))
        else {
            return false;
        };
        self.close_index(idx);
        true
    }

    pub fn model_picker_state(&self) -> Option<&ModelPickerState> {
        self.surfaces
            .iter()
            .rev()
            .find_map(|surface| match &surface.layer {
                SurfaceLayer::ModelPicker(state) => Some(state),
                _ => None,
            })
    }

    pub fn handle_model_picker_key(&mut self, key: TuiKeyEvent) -> ModelPickerOutcome {
        let Some(idx) = self
            .surfaces
            .iter()
            .rposition(|surface| matches!(surface.layer, SurfaceLayer::ModelPicker(_)))
        else {
            return ModelPickerOutcome::Close;
        };
        let outcome = match &mut self.surfaces[idx].layer {
            SurfaceLayer::ModelPicker(state) => state.handle_key(key),
            _ => ModelPickerOutcome::Continue,
        };
        if matches!(
            outcome,
            ModelPickerOutcome::Close | ModelPickerOutcome::Commit { .. }
        ) {
            self.close_index(idx);
        }
        outcome
    }

    pub fn close_model_picker(&mut self) -> bool {
        let Some(idx) = self
            .surfaces
            .iter()
            .rposition(|surface| matches!(surface.layer, SurfaceLayer::ModelPicker(_)))
        else {
            return false;
        };
        self.close_index(idx);
        true
    }

    pub fn active_frame(&self) -> Option<SurfaceFrame> {
        self.focus
            .active()
            .and_then(|id| self.surface(id))
            .or_else(|| self.surfaces.last())
            .map(MountedSurface::frame)
    }

    pub fn frames(&self) -> Vec<SurfaceFrame> {
        self.surfaces.iter().map(MountedSurface::frame).collect()
    }

    pub fn active_canvas_frame(&self) -> Option<vulcan_frontend_api::CanvasFrame> {
        match self.active_frame() {
            Some(SurfaceFrame::FullscreenCanvas(frame)) => Some(frame),
            Some(
                SurfaceFrame::DiffScrubber
                | SurfaceFrame::PausePrompt
                | SurfaceFrame::TextSurface { .. }
                | SurfaceFrame::ModelPicker
                | SurfaceFrame::SessionPicker { .. }
                | SurfaceFrame::ProviderPicker { .. },
            )
            | None => None,
        }
    }

    pub fn handle_canvas_key(&mut self, key: vulcan_frontend_api::CanvasKey) -> bool {
        let Some(idx) = self.active_surface_index() else {
            return false;
        };
        let effects = self.surfaces[idx].handle_key(key);
        self.apply_effects(effects);
        true
    }

    pub fn handle_tick(&mut self) {
        let Some(idx) = self.active_surface_index() else {
            return;
        };
        let effects = self.surfaces[idx].handle_tick();
        self.apply_effects(effects);
    }

    pub fn exit_canvas(&mut self) -> bool {
        let Some(idx) = self
            .surfaces
            .iter()
            .rposition(|surface| matches!(surface.layer, SurfaceLayer::Canvas(_)))
        else {
            return false;
        };
        self.surfaces[idx].exit();
        self.close_index(idx);
        true
    }

    pub fn provider_picker_up(&mut self) {
        if let Some(SurfaceLayer::ProviderPicker(picker)) = self.active_layer_mut() {
            picker.handle_key(TuiKeyEvent::new(
                super::input::TuiKeyCode::Up,
                super::input::TuiKeyModifiers::NONE,
            ));
        }
    }

    pub fn provider_picker_down(&mut self) {
        if let Some(SurfaceLayer::ProviderPicker(picker)) = self.active_layer_mut() {
            picker.handle_key(TuiKeyEvent::new(
                super::input::TuiKeyCode::Down,
                super::input::TuiKeyModifiers::NONE,
            ));
        }
    }

    pub fn selected_provider(&self) -> Option<ProviderPickerEntry> {
        match self.active_layer() {
            Some(SurfaceLayer::ProviderPicker(picker)) => picker.selected(),
            _ => None,
        }
    }

    pub fn close_provider_picker(&mut self) -> bool {
        let Some(idx) = self
            .surfaces
            .iter()
            .rposition(|surface| matches!(surface.layer, SurfaceLayer::ProviderPicker(_)))
        else {
            return false;
        };
        self.close_index(idx);
        true
    }

    pub fn handle_provider_picker_key(&mut self, key: TuiKeyEvent) -> ProviderPickerOutcome {
        let Some(idx) = self
            .surfaces
            .iter()
            .rposition(|surface| matches!(surface.layer, SurfaceLayer::ProviderPicker(_)))
        else {
            return ProviderPickerOutcome::Close;
        };
        let outcome = match &mut self.surfaces[idx].layer {
            SurfaceLayer::ProviderPicker(state) => state.handle_key(key),
            _ => ProviderPickerOutcome::Continue,
        };
        if matches!(
            outcome,
            ProviderPickerOutcome::Close | ProviderPickerOutcome::Commit(_)
        ) {
            self.close_index(idx);
        }
        outcome
    }

    fn mount(&mut self, surface: MountedSurface) {
        let id = surface.spec.id.clone();
        self.surfaces.retain(|mounted| mounted.spec.id != id);
        self.focus.remove(&id);
        let focus = surface.spec.focus;
        self.surfaces.push(surface);
        match focus {
            FocusPolicy::Take => self.focus.focus(id),
            FocusPolicy::Keep => {
                if self.focus.active().is_none() {
                    self.focus.focus(id);
                }
            }
            FocusPolicy::None => {}
        }
    }

    fn next_id(&mut self, prefix: &str) -> SurfaceId {
        self.next_surface += 1;
        SurfaceId::new(format!("{prefix}:{}", self.next_surface))
    }

    fn active_surface_index(&self) -> Option<usize> {
        self.focus
            .active()
            .and_then(|id| self.surface_index(id))
            .or_else(|| self.surfaces.len().checked_sub(1))
    }

    fn active_layer(&self) -> Option<&SurfaceLayer> {
        self.active_surface_index()
            .map(|idx| &self.surfaces[idx].layer)
    }

    fn active_layer_mut(&mut self) -> Option<&mut SurfaceLayer> {
        let idx = self.active_surface_index()?;
        Some(&mut self.surfaces[idx].layer)
    }

    fn surface(&self, id: &SurfaceId) -> Option<&MountedSurface> {
        self.surfaces.iter().find(|surface| surface.spec.id == *id)
    }

    fn surface_index(&self, id: &SurfaceId) -> Option<usize> {
        self.surfaces
            .iter()
            .position(|surface| surface.spec.id == *id)
    }

    fn close_index(&mut self, idx: usize) {
        let surface = self.surfaces.remove(idx);
        self.focus.remove(&surface.spec.id);
    }

    fn apply_effects(&mut self, effects: Vec<UiEffect>) {
        for effect in effects {
            match effect {
                UiEffect::CloseSurface(id) => {
                    if let Some(idx) = self.surface_index(&id) {
                        self.close_index(idx);
                    }
                }
                UiEffect::Focus(id) => {
                    if self.surface_index(&id).is_some() {
                        self.focus.focus(id);
                    }
                }
                UiEffect::Blur => {
                    if let Some(id) = self.focus.active().cloned() {
                        self.focus.remove(&id);
                    }
                }
                UiEffect::RequestRedraw => {}
            }
        }
    }
}

impl MountedSurface {
    fn frame(&self) -> SurfaceFrame {
        match &self.layer {
            SurfaceLayer::Canvas(canvas) => canvas.frame(),
            SurfaceLayer::DiffScrubber(_) => SurfaceFrame::DiffScrubber,
            SurfaceLayer::PausePrompt(_) => SurfaceFrame::PausePrompt,
            SurfaceLayer::Text(surface) => surface.frame(self.spec.placement),
            SurfaceLayer::ModelPicker(_) => SurfaceFrame::ModelPicker,
            SurfaceLayer::SessionPicker(picker) => SurfaceFrame::SessionPicker {
                selection: picker.selection,
            },
            SurfaceLayer::ProviderPicker(picker) => picker.frame(),
        }
    }

    fn handle_key(&mut self, key: vulcan_frontend_api::CanvasKey) -> Vec<UiEffect> {
        let id = self.spec.id.clone();
        if self.spec.close_policy.closes_canvas_key(&key)
            && !matches!(self.layer, SurfaceLayer::Canvas(_))
        {
            return vec![UiEffect::CloseSurface(id)];
        }
        match &mut self.layer {
            SurfaceLayer::Canvas(canvas) => canvas.handle_key(key, id),
            SurfaceLayer::DiffScrubber(_) => Vec::new(),
            SurfaceLayer::PausePrompt(_) => Vec::new(),
            SurfaceLayer::Text(_) => Vec::new(),
            SurfaceLayer::ModelPicker(_) => Vec::new(),
            SurfaceLayer::SessionPicker(_) => Vec::new(),
            SurfaceLayer::ProviderPicker(_) => Vec::new(),
        }
    }

    fn handle_tick(&mut self) -> Vec<UiEffect> {
        let id = self.spec.id.clone();
        match &mut self.layer {
            SurfaceLayer::Canvas(canvas) => canvas.handle_tick(id),
            SurfaceLayer::DiffScrubber(_) => Vec::new(),
            SurfaceLayer::PausePrompt(_) => Vec::new(),
            SurfaceLayer::Text(_) => Vec::new(),
            SurfaceLayer::ModelPicker(_) => Vec::new(),
            SurfaceLayer::SessionPicker(_) => Vec::new(),
            SurfaceLayer::ProviderPicker(_) => Vec::new(),
        }
    }

    fn exit(&mut self) {
        match &mut self.layer {
            SurfaceLayer::Canvas(canvas) => canvas.exit(),
            SurfaceLayer::DiffScrubber(_) => {}
            SurfaceLayer::PausePrompt(_) => {}
            SurfaceLayer::Text(_) => {}
            SurfaceLayer::ModelPicker(_) => {}
            SurfaceLayer::SessionPicker(_) => {}
            SurfaceLayer::ProviderPicker(_) => {}
        }
    }
}

impl CanvasSurface {
    fn frame(&self) -> SurfaceFrame {
        SurfaceFrame::FullscreenCanvas(self.canvas.render())
    }

    fn handle_key(&mut self, key: vulcan_frontend_api::CanvasKey, id: SurfaceId) -> Vec<UiEffect> {
        let control = self.canvas.on_key(key, &self.handle);
        if self.handle.has_exited() || matches!(control, vulcan_frontend_api::CanvasControl::Exit) {
            vec![UiEffect::CloseSurface(id)]
        } else {
            Vec::new()
        }
    }

    fn handle_tick(&mut self, id: SurfaceId) -> Vec<UiEffect> {
        self.canvas.on_tick(&self.handle);
        if self.handle.has_exited() {
            vec![UiEffect::CloseSurface(id)]
        } else {
            Vec::new()
        }
    }

    fn exit(&mut self) {
        self.handle.exit();
    }
}

impl TextSurface {
    fn frame(&self, placement: SurfacePlacement) -> SurfaceFrame {
        SurfaceFrame::TextSurface {
            title: self.title.clone(),
            body: self.body.clone(),
            placement,
        }
    }
}

fn frontend_placement_to_surface_placement(
    placement: vulcan_frontend_api::FrontendSurfacePlacement,
) -> SurfacePlacement {
    match placement {
        vulcan_frontend_api::FrontendSurfacePlacement::Fullscreen => SurfacePlacement::Fullscreen,
        vulcan_frontend_api::FrontendSurfacePlacement::RightDrawer => SurfacePlacement::Drawer {
            edge: super::surface::DrawerEdge::Right,
            size: 64,
        },
        vulcan_frontend_api::FrontendSurfacePlacement::BottomDrawer => SurfacePlacement::Drawer {
            edge: super::surface::DrawerEdge::Bottom,
            size: 16,
        },
        vulcan_frontend_api::FrontendSurfacePlacement::Modal => SurfacePlacement::Modal {
            width: 88,
            height: 24,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestCanvas;

    impl vulcan_frontend_api::Canvas for TestCanvas {
        fn render(&self) -> vulcan_frontend_api::CanvasFrame {
            vulcan_frontend_api::CanvasFrame {
                title: "Test canvas".into(),
                lines: vec!["alive".into()],
            }
        }
    }

    fn test_request() -> vulcan_frontend_api::CanvasRequest {
        let mut ui = vulcan_frontend_api::ExtensionUi::default();
        ui.custom(vulcan_frontend_api::CanvasFactory::new(|_handle| {
            Box::new(TestCanvas)
        }))
        .expect("canvas request");
        ui.drain_canvas_requests().pop().expect("request")
    }

    #[test]
    fn canvas_surface_renders_and_exits_on_key() {
        let mut runtime = UiRuntime::default();
        runtime.mount_canvas(test_request());

        assert_eq!(
            runtime.active_canvas_frame().expect("frame").title,
            "Test canvas"
        );
        assert!(runtime.handle_canvas_key(vulcan_frontend_api::CanvasKey::Esc));
        assert!(!runtime.has_canvas());
    }

    #[test]
    fn exit_canvas_closes_active_canvas() {
        let mut runtime = UiRuntime::default();
        runtime.mount_canvas(test_request());

        assert!(runtime.exit_canvas());
        assert!(!runtime.has_canvas());
        assert!(!runtime.exit_canvas());
    }

    #[test]
    fn provider_picker_tracks_selection_and_selected_entry() {
        let mut runtime = UiRuntime::default();
        runtime.open_provider_picker(
            vec![
                ProviderPickerEntry {
                    name: None,
                    model: "default-model".into(),
                    base_url: "https://default.test".into(),
                },
                ProviderPickerEntry {
                    name: Some("fast".into()),
                    model: "fast-model".into(),
                    base_url: "https://fast.test".into(),
                },
            ],
            0,
        );

        assert!(runtime.has_provider_picker());
        runtime.provider_picker_down();
        assert_eq!(
            runtime
                .selected_provider()
                .expect("selected")
                .name
                .as_deref(),
            Some("fast")
        );
        runtime.provider_picker_up();
        assert_eq!(runtime.selected_provider().expect("selected").name, None);
        assert!(runtime.close_provider_picker());
        assert!(!runtime.has_provider_picker());
    }

    #[test]
    fn text_surface_mounts_as_active_frame_and_closes() {
        let mut runtime = UiRuntime::default();
        runtime.open_text_surface(vulcan_frontend_api::FrontendSurface::modal(
            "todo",
            "Todos",
            vec!["one".into(), "two".into()],
        ));

        assert!(matches!(
            runtime.active_frame(),
            Some(SurfaceFrame::TextSurface {
                title,
                body,
                placement: SurfacePlacement::Modal { .. },
            })
                if title == "Todos" && body == vec!["one".to_string(), "two".to_string()]
        ));
        assert!(runtime.close_text_surface());
        assert!(runtime.active_frame().is_none());
    }

    #[test]
    fn text_surface_frame_preserves_requested_placement() {
        let mut runtime = UiRuntime::default();
        runtime.open_text_surface(vulcan_frontend_api::FrontendSurface {
            id: "drawer".into(),
            title: "Drawer".into(),
            body: vec!["body".into()],
            placement: vulcan_frontend_api::FrontendSurfacePlacement::RightDrawer,
        });

        assert!(matches!(
            runtime.active_frame(),
            Some(SurfaceFrame::TextSurface {
                placement: SurfacePlacement::Drawer {
                    edge: super::super::surface::DrawerEdge::Right,
                    ..
                },
                ..
            })
        ));
    }

    #[test]
    fn text_surface_updates_and_closes_by_id() {
        let mut runtime = UiRuntime::default();
        runtime.open_text_surface(vulcan_frontend_api::FrontendSurface::modal(
            "todo",
            "Todos",
            vec!["one".into()],
        ));

        assert!(
            runtime.update_text_surface(vulcan_frontend_api::FrontendSurfaceUpdate {
                id: "todo".into(),
                title: Some("Updated".into()),
                body: Some(vec!["two".into()]),
                placement: Some(vulcan_frontend_api::FrontendSurfacePlacement::BottomDrawer),
            },)
        );
        assert!(matches!(
            runtime.active_frame(),
            Some(SurfaceFrame::TextSurface {
                title,
                body,
                placement: SurfacePlacement::Drawer {
                    edge: super::super::surface::DrawerEdge::Bottom,
                    ..
                },
            }) if title == "Updated" && body == vec!["two".to_string()]
        ));
        assert!(runtime.close_surface("todo"));
        assert!(runtime.active_frame().is_none());
    }

    #[test]
    fn text_surface_closes_on_escape_key_event() {
        let mut runtime = UiRuntime::default();
        runtime.open_text_surface(vulcan_frontend_api::FrontendSurface::modal(
            "todo",
            "Todos",
            vec!["one".into()],
        ));

        assert!(runtime.handle_canvas_key(vulcan_frontend_api::CanvasKey::Esc));
        assert!(runtime.active_frame().is_none());
    }

    #[test]
    fn modal_surface_policy_marks_text_surface_as_cancel_blocker() {
        let mut runtime = UiRuntime::default();
        runtime.open_text_surface(vulcan_frontend_api::FrontendSurface::modal(
            "todo",
            "Todos",
            vec!["one".into()],
        ));

        assert!(runtime.has_modal_blocking_surface());
        runtime.handle_canvas_key(vulcan_frontend_api::CanvasKey::Esc);
        assert!(!runtime.has_modal_blocking_surface());
    }
}
