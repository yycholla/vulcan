//! Diff scrubber surface state.

use crate::pause::{AgentPause, DiffScrubHunk};
use crate::tui::input::{TuiKeyCode, TuiKeyEvent};

#[derive(Debug)]
pub struct DiffScrubberState {
    pub path: String,
    pub hunks: Vec<DiffScrubHunk>,
    pub accepted: Vec<bool>,
    pub selection: usize,
    pause: Option<AgentPause>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DiffScrubberAction {
    Continue,
    Accept(Vec<usize>),
    Cancel,
}

impl DiffScrubberState {
    pub fn new(path: String, hunks: Vec<DiffScrubHunk>, pause: AgentPause) -> Self {
        let accepted = vec![true; hunks.len()];
        Self {
            path,
            hunks,
            accepted,
            selection: 0,
            pause: Some(pause),
        }
    }

    pub fn take_pause(&mut self) -> Option<AgentPause> {
        self.pause.take()
    }

    pub fn handle_key(&mut self, key: TuiKeyEvent) -> DiffScrubberAction {
        let total = self.hunks.len();
        match key.code {
            TuiKeyCode::Up | TuiKeyCode::Char('k') => {
                self.selection = self.selection.saturating_sub(1);
            }
            TuiKeyCode::Down | TuiKeyCode::Char('j') => {
                self.selection = self
                    .selection
                    .saturating_add(1)
                    .min(total.saturating_sub(1));
            }
            TuiKeyCode::Char('y') => {
                if let Some(slot) = self.accepted.get_mut(self.selection) {
                    *slot = !*slot;
                }
            }
            TuiKeyCode::Char('Y') => {
                for slot in &mut self.accepted {
                    *slot = true;
                }
            }
            TuiKeyCode::Char('n') => {
                if let Some(slot) = self.accepted.get_mut(self.selection) {
                    *slot = false;
                }
            }
            TuiKeyCode::Char('N') => {
                for slot in &mut self.accepted {
                    *slot = false;
                }
            }
            TuiKeyCode::Enter => {
                let indices = self
                    .accepted
                    .iter()
                    .enumerate()
                    .filter_map(|(i, ok)| if *ok { Some(i) } else { None })
                    .collect();
                return DiffScrubberAction::Accept(indices);
            }
            TuiKeyCode::Esc => return DiffScrubberAction::Cancel,
            _ => {}
        }
        DiffScrubberAction::Continue
    }
}

pub struct DiffScrubberOutcome {
    pub pause: Option<AgentPause>,
    pub action: DiffScrubberAction,
    pub total: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pause::{AgentResume, PauseKind};
    use crate::tui::input::TuiKeyModifiers;
    use tokio::sync::oneshot;

    fn state() -> DiffScrubberState {
        let (reply, _rx) = oneshot::channel::<AgentResume>();
        DiffScrubberState::new(
            "src/lib.rs".into(),
            vec![
                DiffScrubHunk {
                    offset: 0,
                    line_no: 1,
                    before_lines: vec!["one".into()],
                    after_lines: vec!["ONE".into()],
                },
                DiffScrubHunk {
                    offset: 8,
                    line_no: 2,
                    before_lines: vec!["two".into()],
                    after_lines: vec!["TWO".into()],
                },
            ],
            AgentPause {
                kind: PauseKind::DiffScrub {
                    path: "src/lib.rs".into(),
                    hunks: Vec::new(),
                },
                reply,
                options: Vec::new(),
            },
        )
    }

    #[test]
    fn toggles_and_accepts_selected_hunks() {
        let mut state = state();
        assert_eq!(
            state.handle_key(TuiKeyEvent::new(
                TuiKeyCode::Char('n'),
                TuiKeyModifiers::NONE
            )),
            DiffScrubberAction::Continue
        );
        assert_eq!(
            state.handle_key(TuiKeyEvent::new(TuiKeyCode::Enter, TuiKeyModifiers::NONE)),
            DiffScrubberAction::Accept(vec![1])
        );
    }
}
