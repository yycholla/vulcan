use termwiz::input as termwiz_input;
use tui_textarea::{Input as TextInput, Key as TextKey};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TuiInputEvent {
    Key(TuiKeyEvent),
    Paste(String),
    Mouse(TuiMouseEvent),
    Resize { cols: u16, rows: u16 },
    Wake,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TuiKeyEvent {
    pub code: TuiKeyCode,
    pub modifiers: TuiKeyModifiers,
}

impl TuiKeyEvent {
    pub fn new(code: TuiKeyCode, modifiers: TuiKeyModifiers) -> Self {
        Self { code, modifiers }
    }

    pub fn ctrl_char(c: char) -> Self {
        Self::new(TuiKeyCode::Char(c), TuiKeyModifiers::CONTROL)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TuiKeyCode {
    Char(char),
    F(u8),
    Esc,
    Enter,
    Tab,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Delete,
    PageUp,
    PageDown,
    Other,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TuiKeyModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl TuiKeyModifiers {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
    };
    pub const CONTROL: Self = Self {
        ctrl: true,
        alt: false,
        shift: false,
    };
    pub const ALT: Self = Self {
        ctrl: false,
        alt: true,
        shift: false,
    };
    pub const SHIFT: Self = Self {
        ctrl: false,
        alt: false,
        shift: true,
    };

    pub fn contains(self, other: Self) -> bool {
        (!other.ctrl || self.ctrl) && (!other.alt || self.alt) && (!other.shift || self.shift)
    }

    pub fn insert(&mut self, other: Self) {
        self.ctrl |= other.ctrl;
        self.alt |= other.alt;
        self.shift |= other.shift;
    }

    pub fn is_empty(self) -> bool {
        !self.ctrl && !self.alt && !self.shift
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TuiMouseEvent {
    pub kind: TuiMouseEventKind,
    pub x: u16,
    pub y: u16,
    pub modifiers: TuiKeyModifiers,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TuiMouseEventKind {
    ScrollUp,
    ScrollDown,
    Other,
}

impl From<termwiz_input::InputEvent> for TuiInputEvent {
    fn from(event: termwiz_input::InputEvent) -> Self {
        match event {
            termwiz_input::InputEvent::Key(key) => Self::Key(TuiKeyEvent::from(key)),
            termwiz_input::InputEvent::Paste(text) => Self::Paste(text),
            termwiz_input::InputEvent::Mouse(mouse) => Self::Mouse(TuiMouseEvent::from(mouse)),
            termwiz_input::InputEvent::PixelMouse(_) => Self::Unsupported,
            termwiz_input::InputEvent::Resized { cols, rows } => Self::Resize {
                cols: cols.min(u16::MAX as usize) as u16,
                rows: rows.min(u16::MAX as usize) as u16,
            },
            termwiz_input::InputEvent::Wake => Self::Wake,
        }
    }
}

impl From<termwiz_input::KeyEvent> for TuiKeyEvent {
    fn from(event: termwiz_input::KeyEvent) -> Self {
        Self {
            code: TuiKeyCode::from(event.key),
            modifiers: TuiKeyModifiers::from(event.modifiers),
        }
    }
}

impl From<termwiz_input::KeyCode> for TuiKeyCode {
    fn from(code: termwiz_input::KeyCode) -> Self {
        match code {
            termwiz_input::KeyCode::Char(c) => Self::Char(c),
            termwiz_input::KeyCode::Function(n) => Self::F(n),
            termwiz_input::KeyCode::Escape => Self::Esc,
            termwiz_input::KeyCode::Enter => Self::Enter,
            termwiz_input::KeyCode::Tab => Self::Tab,
            termwiz_input::KeyCode::Backspace => Self::Backspace,
            termwiz_input::KeyCode::UpArrow => Self::Up,
            termwiz_input::KeyCode::DownArrow => Self::Down,
            termwiz_input::KeyCode::LeftArrow => Self::Left,
            termwiz_input::KeyCode::RightArrow => Self::Right,
            termwiz_input::KeyCode::Home => Self::Home,
            termwiz_input::KeyCode::End => Self::End,
            termwiz_input::KeyCode::Delete => Self::Delete,
            termwiz_input::KeyCode::PageUp => Self::PageUp,
            termwiz_input::KeyCode::PageDown => Self::PageDown,
            _ => Self::Other,
        }
    }
}

impl From<termwiz_input::Modifiers> for TuiKeyModifiers {
    fn from(modifiers: termwiz_input::Modifiers) -> Self {
        Self {
            ctrl: modifiers.contains(termwiz_input::Modifiers::CTRL),
            alt: modifiers.contains(termwiz_input::Modifiers::ALT),
            shift: modifiers.contains(termwiz_input::Modifiers::SHIFT),
        }
    }
}

impl From<termwiz_input::MouseEvent> for TuiMouseEvent {
    fn from(event: termwiz_input::MouseEvent) -> Self {
        Self {
            kind: mouse_kind(event.mouse_buttons),
            x: event.x,
            y: event.y,
            modifiers: TuiKeyModifiers::from(event.modifiers),
        }
    }
}

fn mouse_kind(buttons: termwiz_input::MouseButtons) -> TuiMouseEventKind {
    if buttons.contains(termwiz_input::MouseButtons::VERT_WHEEL) {
        if buttons.contains(termwiz_input::MouseButtons::WHEEL_POSITIVE) {
            TuiMouseEventKind::ScrollUp
        } else {
            TuiMouseEventKind::ScrollDown
        }
    } else {
        TuiMouseEventKind::Other
    }
}

impl From<TuiKeyEvent> for TextInput {
    fn from(event: TuiKeyEvent) -> Self {
        Self {
            key: TextKey::from(event.code),
            ctrl: event.modifiers.ctrl,
            alt: event.modifiers.alt,
            shift: event.modifiers.shift,
        }
    }
}

impl From<TuiKeyCode> for TextKey {
    fn from(code: TuiKeyCode) -> Self {
        match code {
            TuiKeyCode::Char(c) => Self::Char(c),
            TuiKeyCode::F(n) => Self::F(n),
            TuiKeyCode::Esc => Self::Esc,
            TuiKeyCode::Enter => Self::Enter,
            TuiKeyCode::Tab => Self::Tab,
            TuiKeyCode::Backspace => Self::Backspace,
            TuiKeyCode::Up => Self::Up,
            TuiKeyCode::Down => Self::Down,
            TuiKeyCode::Left => Self::Left,
            TuiKeyCode::Right => Self::Right,
            TuiKeyCode::Home => Self::Home,
            TuiKeyCode::End => Self::End,
            TuiKeyCode::Delete => Self::Delete,
            TuiKeyCode::PageUp => Self::PageUp,
            TuiKeyCode::PageDown => Self::PageDown,
            TuiKeyCode::Other => Self::Null,
        }
    }
}
