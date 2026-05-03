//! Focus stack for mounted TUI surfaces.

use super::surface::SurfaceId;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FocusStack {
    active: Option<SurfaceId>,
    stack: Vec<SurfaceId>,
}

impl FocusStack {
    pub fn active(&self) -> Option<&SurfaceId> {
        self.active.as_ref()
    }

    pub fn focus(&mut self, id: SurfaceId) {
        if self.active.as_ref() == Some(&id) {
            return;
        }
        if let Some(current) = self.active.replace(id) {
            self.stack.retain(|stacked| stacked != &current);
            self.stack.push(current);
        }
    }

    pub fn blur(&mut self) -> Option<SurfaceId> {
        self.active = self.stack.pop();
        self.active.clone()
    }

    pub fn remove(&mut self, id: &SurfaceId) {
        if self.active.as_ref() == Some(id) {
            self.blur();
        }
        self.stack.retain(|stacked| stacked != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_pushes_and_restores_previous_surface() {
        let mut focus = FocusStack::default();
        let first = SurfaceId::new("first");
        let second = SurfaceId::new("second");

        focus.focus(first.clone());
        focus.focus(second.clone());

        assert_eq!(focus.active(), Some(&second));
        assert_eq!(focus.blur(), Some(first.clone()));
        assert_eq!(focus.active(), Some(&first));
    }

    #[test]
    fn remove_active_restores_previous_surface() {
        let mut focus = FocusStack::default();
        let first = SurfaceId::new("first");
        let second = SurfaceId::new("second");

        focus.focus(first.clone());
        focus.focus(second.clone());
        focus.remove(&second);

        assert_eq!(focus.active(), Some(&first));
    }
}
