//! Prompt editing state.
//!
//! The prompt editor owns text editing and Vim-mode behavior. `AppState`
//! mirrors the editor text into its legacy `input` string until prompt
//! submission and slash-command routing are moved behind this module too.

use tui_textarea::{CursorMove, Input, Key, TextArea};

use crate::tui::input::TuiKeyEvent;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PromptEditMode {
    Normal,
    #[default]
    Insert,
}

impl PromptEditMode {
    pub fn badge(self) -> &'static str {
        match self {
            PromptEditMode::Normal => "NORMAL",
            PromptEditMode::Insert => "INSERT",
        }
    }
}

#[derive(Clone, Debug)]
pub struct PromptEditor {
    textarea: TextArea<'static>,
    mode: PromptEditMode,
    pending: Input,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptEnterIntent {
    Empty,
    Edit,
    Submit(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptEscapeIntent {
    ClearCommand,
    Edit,
    Exit,
}

impl Default for PromptEditor {
    fn default() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_cursor_line_style(ratatui::style::Style::default());
        Self {
            textarea,
            mode: PromptEditMode::Insert,
            pending: Input::default(),
        }
    }
}

impl PromptEditor {
    pub fn mode(&self) -> PromptEditMode {
        self.mode
    }

    pub fn textarea(&self) -> &TextArea<'static> {
        &self.textarea
    }

    pub fn text(&self) -> String {
        self.textarea.lines().join("\n")
    }

    pub fn enter_intent(&self) -> PromptEnterIntent {
        let text = self.text();
        if text.is_empty() {
            PromptEnterIntent::Empty
        } else {
            PromptEnterIntent::Submit(text)
        }
    }

    pub fn escape_intent(&self) -> PromptEscapeIntent {
        let text = self.text();
        if text.starts_with('/') {
            PromptEscapeIntent::ClearCommand
        } else if self.mode == PromptEditMode::Insert {
            PromptEscapeIntent::Edit
        } else {
            PromptEscapeIntent::Exit
        }
    }

    pub fn set_text(&mut self, text: impl AsRef<str>) {
        let mut lines = text
            .as_ref()
            .split('\n')
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        if lines.is_empty() {
            lines.push(String::new());
        }
        let row = lines.len().saturating_sub(1);
        let col = lines.last().map(|line| line.chars().count()).unwrap_or(0);
        self.textarea.set_lines(lines, (row, col));
        self.pending = Input::default();
    }

    pub fn clear(&mut self) {
        self.set_text("");
        self.mode = PromptEditMode::Insert;
    }

    pub fn insert_str(&mut self, text: &str) {
        self.textarea.insert_str(text);
        self.pending = Input::default();
    }

    pub fn handle_key(&mut self, key: TuiKeyEvent) -> bool {
        let input = Input::from(key);
        if input.key == Key::Null {
            return false;
        }

        let changed = match self.mode {
            PromptEditMode::Insert => self.handle_insert(input),
            PromptEditMode::Normal => self.handle_normal(input),
        };
        if changed {
            self.pending = Input::default();
        }
        changed
    }

    fn handle_insert(&mut self, input: Input) -> bool {
        match input {
            Input { key: Key::Esc, .. } => {
                self.mode = PromptEditMode::Normal;
                true
            }
            Input {
                key: Key::Enter,
                shift: false,
                ..
            } => false,
            input => self.textarea.input(input),
        }
    }

    fn handle_normal(&mut self, input: Input) -> bool {
        match input {
            Input {
                key: Key::Char('h'),
                ..
            } => self.textarea.move_cursor(CursorMove::Back),
            Input {
                key: Key::Char('j'),
                ..
            } => self.textarea.move_cursor(CursorMove::Down),
            Input {
                key: Key::Char('k'),
                ..
            } => self.textarea.move_cursor(CursorMove::Up),
            Input {
                key: Key::Char('l'),
                ..
            } => self.textarea.move_cursor(CursorMove::Forward),
            Input {
                key: Key::Char('w'),
                ..
            } => self.textarea.move_cursor(CursorMove::WordForward),
            Input {
                key: Key::Char('e'),
                ctrl: false,
                ..
            } => self.textarea.move_cursor(CursorMove::WordEnd),
            Input {
                key: Key::Char('b'),
                ctrl: false,
                ..
            } => self.textarea.move_cursor(CursorMove::WordBack),
            Input {
                key: Key::Char('^'),
                ..
            } => self.textarea.move_cursor(CursorMove::Head),
            Input {
                key: Key::Char('$'),
                ..
            } => self.textarea.move_cursor(CursorMove::End),
            Input {
                key: Key::Char('G'),
                ctrl: false,
                ..
            } => self.textarea.move_cursor(CursorMove::Bottom),
            Input {
                key: Key::Char('g'),
                ctrl: false,
                ..
            } if matches!(
                self.pending,
                Input {
                    key: Key::Char('g'),
                    ctrl: false,
                    ..
                }
            ) =>
            {
                self.textarea.move_cursor(CursorMove::Top);
                self.pending = Input::default();
                return true;
            }
            Input {
                key: Key::Char('i'),
                ..
            } => self.mode = PromptEditMode::Insert,
            Input {
                key: Key::Char('a'),
                ..
            } => {
                self.textarea.move_cursor(CursorMove::Forward);
                self.mode = PromptEditMode::Insert;
            }
            Input {
                key: Key::Char('A'),
                ..
            } => {
                self.textarea.move_cursor(CursorMove::End);
                self.mode = PromptEditMode::Insert;
            }
            Input {
                key: Key::Char('o'),
                ..
            } => {
                self.textarea.move_cursor(CursorMove::End);
                self.textarea.insert_newline();
                self.mode = PromptEditMode::Insert;
            }
            Input {
                key: Key::Char('O'),
                ..
            } => {
                self.textarea.move_cursor(CursorMove::Head);
                self.textarea.insert_newline();
                self.textarea.move_cursor(CursorMove::Up);
                self.mode = PromptEditMode::Insert;
            }
            Input {
                key: Key::Char('x'),
                ..
            } => {
                self.textarea.delete_next_char();
            }
            Input {
                key: Key::Char('D'),
                ..
            } => {
                self.textarea.delete_line_by_end();
            }
            Input {
                key: Key::Char('u'),
                ctrl: false,
                ..
            } => {
                self.textarea.undo();
            }
            Input {
                key: Key::Char('r'),
                ctrl: true,
                ..
            } => {
                self.textarea.redo();
            }
            input => {
                self.pending = input;
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use crate::tui::input::{TuiKeyCode, TuiKeyModifiers};

    use super::*;

    #[test]
    fn insert_mode_uses_shift_enter_for_multiline_text() {
        let mut editor = PromptEditor::default();

        editor.insert_str("first");
        assert!(!editor.handle_key(TuiKeyEvent::new(TuiKeyCode::Enter, TuiKeyModifiers::NONE)));
        assert!(editor.handle_key(TuiKeyEvent::new(TuiKeyCode::Enter, TuiKeyModifiers::SHIFT)));
        editor.insert_str("second");

        assert_eq!(editor.text(), "first\nsecond");
        assert_eq!(editor.mode(), PromptEditMode::Insert);
    }

    #[test]
    fn escape_enters_normal_mode_and_i_returns_to_insert() {
        let mut editor = PromptEditor::default();

        editor.insert_str("hello");
        assert!(editor.handle_key(TuiKeyEvent::new(TuiKeyCode::Esc, TuiKeyModifiers::NONE)));
        assert_eq!(editor.mode(), PromptEditMode::Normal);

        assert!(editor.handle_key(TuiKeyEvent::new(
            TuiKeyCode::Char('i'),
            TuiKeyModifiers::NONE
        )));
        assert_eq!(editor.mode(), PromptEditMode::Insert);
    }

    #[test]
    fn enter_intent_submits_non_empty_prompt() {
        let mut editor = PromptEditor::default();

        assert_eq!(editor.enter_intent(), PromptEnterIntent::Empty);

        editor.insert_str("hello");
        assert_eq!(
            editor.enter_intent(),
            PromptEnterIntent::Submit("hello".into())
        );

        editor.set_text("/help");
        assert_eq!(
            editor.enter_intent(),
            PromptEnterIntent::Submit("/help".into())
        );
    }

    #[test]
    fn escape_intent_clears_command_edits_insert_and_exits_normal() {
        let mut editor = PromptEditor::default();

        editor.set_text("/help");
        assert_eq!(editor.escape_intent(), PromptEscapeIntent::ClearCommand);

        editor.set_text("hello");
        assert_eq!(editor.escape_intent(), PromptEscapeIntent::Edit);

        assert!(editor.handle_key(TuiKeyEvent::new(TuiKeyCode::Esc, TuiKeyModifiers::NONE)));
        assert_eq!(editor.escape_intent(), PromptEscapeIntent::Exit);
    }
}
