//! Vulcan surface vocabulary.
//!
//! Ratatui remains immediate-mode: every frame is still rendered from state.
//! These types describe retained transient UI surfaces that the `UiRuntime`
//! mounts, focuses, places, and routes events to.

use super::picker_state::ProviderPickerEntry;
use ratatui::layout::Rect;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SurfaceId(String);

impl SurfaceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for SurfaceId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for SurfaceId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum SurfaceKind {
    Fullscreen,
    Modal,
    Drawer,
    Popup,
    Status,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceSpec {
    pub id: SurfaceId,
    pub title: String,
    pub kind: SurfaceKind,
    pub placement: SurfacePlacement,
    pub focus: FocusPolicy,
    pub close_policy: SurfaceClosePolicy,
}

impl SurfaceSpec {
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        kind: SurfaceKind,
        placement: SurfacePlacement,
    ) -> Self {
        Self {
            id: SurfaceId::new(id),
            title: title.into(),
            kind,
            placement,
            focus: FocusPolicy::Take,
            close_policy: SurfaceClosePolicy::default(),
        }
    }

    pub fn with_close_policy(mut self, close_policy: SurfaceClosePolicy) -> Self {
        self.close_policy = close_policy;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceClosePolicy {
    pub esc_closes: bool,
    pub ctrl_c_closes: bool,
    pub modal_blocks: bool,
    pub deny_on_close: bool,
}

impl SurfaceClosePolicy {
    pub const fn passive() -> Self {
        Self {
            esc_closes: false,
            ctrl_c_closes: false,
            modal_blocks: false,
            deny_on_close: false,
        }
    }

    pub const fn modal() -> Self {
        Self {
            esc_closes: true,
            ctrl_c_closes: true,
            modal_blocks: true,
            deny_on_close: false,
        }
    }

    pub const fn approval() -> Self {
        Self {
            esc_closes: true,
            ctrl_c_closes: true,
            modal_blocks: true,
            deny_on_close: true,
        }
    }

    pub fn closes_canvas_key(self, key: &vulcan_frontend_api::CanvasKey) -> bool {
        matches!(key, vulcan_frontend_api::CanvasKey::Esc) && self.esc_closes
            || matches!(key, vulcan_frontend_api::CanvasKey::CtrlC) && self.ctrl_c_closes
    }
}

impl Default for SurfaceClosePolicy {
    fn default() -> Self {
        Self::passive()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)]
pub enum FocusPolicy {
    #[default]
    Take,
    Keep,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum DrawerEdge {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub enum SurfacePlacement {
    Fullscreen,
    Modal { width: u16, height: u16 },
    Drawer { edge: DrawerEdge, size: u16 },
}

pub fn resolve_surface_area(container: Rect, placement: SurfacePlacement) -> Rect {
    match placement {
        SurfacePlacement::Fullscreen => container,
        SurfacePlacement::Modal { width, height } => {
            let width = width.min(container.width);
            let height = height.min(container.height);
            Rect {
                x: container.x + container.width.saturating_sub(width) / 2,
                y: container.y + container.height.saturating_sub(height) / 2,
                width,
                height,
            }
        }
        SurfacePlacement::Drawer { edge, size } => match edge {
            DrawerEdge::Left => Rect {
                x: container.x,
                y: container.y,
                width: size.min(container.width),
                height: container.height,
            },
            DrawerEdge::Right => {
                let width = size.min(container.width);
                Rect {
                    x: container.x + container.width.saturating_sub(width),
                    y: container.y,
                    width,
                    height: container.height,
                }
            }
            DrawerEdge::Top => Rect {
                x: container.x,
                y: container.y,
                width: container.width,
                height: size.min(container.height),
            },
            DrawerEdge::Bottom => {
                let height = size.min(container.height);
                Rect {
                    x: container.x,
                    y: container.y + container.height.saturating_sub(height),
                    width: container.width,
                    height,
                }
            }
        },
    }
}

#[derive(Clone)]
pub enum SurfaceFrame {
    FullscreenCanvas(vulcan_frontend_api::CanvasFrame),
    DiffScrubber,
    PausePrompt,
    TextSurface {
        title: String,
        body: Vec<String>,
        placement: SurfacePlacement,
    },
    ModelPicker,
    SessionPicker {
        selection: usize,
    },
    ProviderPicker {
        items: Vec<ProviderPickerEntry>,
        selection: usize,
    },
}

#[allow(dead_code)]
pub enum UiEvent {
    Key(vulcan_frontend_api::CanvasKey),
    Tick,
    Close,
}

#[allow(dead_code)]
pub enum UiEffect {
    CloseSurface(SurfaceId),
    Focus(SurfaceId),
    Blur,
    RequestRedraw,
}

#[allow(dead_code)]
pub trait UiSurface {
    fn spec(&self) -> &SurfaceSpec;
    fn frame(&self) -> SurfaceFrame;
    fn handle_event(&mut self, event: UiEvent) -> Vec<UiEffect>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modal_placement_centers_and_clamps_to_container() {
        let container = Rect {
            x: 10,
            y: 5,
            width: 100,
            height: 40,
        };

        assert_eq!(
            resolve_surface_area(
                container,
                SurfacePlacement::Modal {
                    width: 40,
                    height: 12
                }
            ),
            Rect {
                x: 40,
                y: 19,
                width: 40,
                height: 12,
            }
        );

        assert_eq!(
            resolve_surface_area(
                container,
                SurfacePlacement::Modal {
                    width: 200,
                    height: 80
                }
            ),
            container
        );
    }

    #[test]
    fn drawer_placement_respects_edges_and_clamps_size() {
        let container = Rect {
            x: 2,
            y: 3,
            width: 80,
            height: 24,
        };

        assert_eq!(
            resolve_surface_area(
                container,
                SurfacePlacement::Drawer {
                    edge: DrawerEdge::Right,
                    size: 20,
                }
            ),
            Rect {
                x: 62,
                y: 3,
                width: 20,
                height: 24,
            }
        );

        assert_eq!(
            resolve_surface_area(
                container,
                SurfacePlacement::Drawer {
                    edge: DrawerEdge::Bottom,
                    size: 40,
                }
            ),
            Rect {
                x: 2,
                y: 3,
                width: 80,
                height: 24,
            }
        );
    }
}
