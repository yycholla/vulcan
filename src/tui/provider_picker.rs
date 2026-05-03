use super::input::{TuiKeyCode, TuiKeyEvent};
use super::picker_state::ProviderPickerEntry;
use super::surface::SurfaceFrame;

pub enum ProviderPickerOutcome {
    Continue,
    Close,
    Commit(ProviderPickerEntry),
}

pub struct ProviderPickerState {
    items: Vec<ProviderPickerEntry>,
    selection: usize,
}

impl ProviderPickerState {
    pub fn new(items: Vec<ProviderPickerEntry>, selection: usize) -> Self {
        let selection = selection.min(items.len().saturating_sub(1));
        Self { items, selection }
    }

    pub fn items(&self) -> &[ProviderPickerEntry] {
        &self.items
    }

    pub fn selection(&self) -> usize {
        self.selection
    }

    pub fn selected(&self) -> Option<ProviderPickerEntry> {
        let idx = self.selection.min(self.items.len().saturating_sub(1));
        self.items.get(idx).cloned()
    }

    pub fn frame(&self) -> SurfaceFrame {
        SurfaceFrame::ProviderPicker {
            items: self.items.clone(),
            selection: self.selection,
        }
    }

    pub fn handle_key(&mut self, key: TuiKeyEvent) -> ProviderPickerOutcome {
        match key.code {
            TuiKeyCode::Up | TuiKeyCode::Char('k') => {
                self.move_up();
                ProviderPickerOutcome::Continue
            }
            TuiKeyCode::Down | TuiKeyCode::Char('j') => {
                self.move_down();
                ProviderPickerOutcome::Continue
            }
            TuiKeyCode::Enter => self
                .selected()
                .map(ProviderPickerOutcome::Commit)
                .unwrap_or(ProviderPickerOutcome::Close),
            TuiKeyCode::Esc => ProviderPickerOutcome::Close,
            _ => ProviderPickerOutcome::Continue,
        }
    }

    fn move_up(&mut self) {
        self.selection = self.selection.saturating_sub(1);
    }

    fn move_down(&mut self) {
        let max = self.items.len().saturating_sub(1);
        self.selection = self.selection.saturating_add(1).min(max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::input::TuiKeyModifiers;

    fn entry(name: &str) -> ProviderPickerEntry {
        ProviderPickerEntry {
            name: Some(name.to_string()),
            model: "model".to_string(),
            base_url: "http://localhost".to_string(),
        }
    }

    fn key(code: TuiKeyCode) -> TuiKeyEvent {
        TuiKeyEvent::new(code, TuiKeyModifiers::NONE)
    }

    #[test]
    fn navigation_clamps_to_available_items() {
        let mut state = ProviderPickerState::new(vec![entry("a"), entry("b")], 0);

        state.handle_key(key(TuiKeyCode::Up));
        assert_eq!(state.selection(), 0);

        state.handle_key(key(TuiKeyCode::Down));
        state.handle_key(key(TuiKeyCode::Down));
        assert_eq!(state.selection(), 1);
    }

    #[test]
    fn enter_commits_selected_provider() {
        let mut state = ProviderPickerState::new(vec![entry("a"), entry("b")], 1);

        let outcome = state.handle_key(key(TuiKeyCode::Enter));

        assert!(matches!(
            outcome,
            ProviderPickerOutcome::Commit(ProviderPickerEntry {
                name: Some(name),
                ..
            }) if name == "b"
        ));
    }
}
