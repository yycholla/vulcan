//! Terminal lifecycle helpers extracted from `tui/mod.rs` (YYC-108).
//!
//! `init_terminal` creates the Ratatui terminal session and captures
//! terminal capabilities. The Termwiz backend owns raw mode and
//! alternate-screen lifecycle, restoring both when dropped.

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::TermwizBackend;
use termwiz::caps::{Capabilities, ColorLevel, ProbeHints};
use termwiz::input::InputEvent;
use termwiz::terminal::{SystemTerminal, Terminal as TermwizTerminal, buffered::BufferedTerminal};

const ENABLE_MOUSE_CAPTURE_BY_DEFAULT: bool = false;

fn mouse_capture_enabled_by_default() -> bool {
    ENABLE_MOUSE_CAPTURE_BY_DEFAULT
}

pub(super) type TuiTerminal = Terminal<TermwizBackend>;
pub(super) type TuiInputTerminal = SystemTerminal;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TerminalCapabilities {
    pub color_level: TerminalColorLevel,
    pub bracketed_paste: bool,
    pub hyperlinks: bool,
    pub sixel: bool,
    pub iterm2_image: bool,
    pub mouse_reporting: bool,
    pub background_color_erase: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TerminalColorLevel {
    Monochrome,
    Sixteen,
    TwoFiftySix,
    TrueColor,
}

impl TerminalCapabilities {
    fn from_termwiz(caps: &Capabilities) -> Self {
        Self {
            color_level: match caps.color_level() {
                ColorLevel::MonoChrome => TerminalColorLevel::Monochrome,
                ColorLevel::Sixteen => TerminalColorLevel::Sixteen,
                ColorLevel::TwoFiftySix => TerminalColorLevel::TwoFiftySix,
                ColorLevel::TrueColor => TerminalColorLevel::TrueColor,
            },
            bracketed_paste: caps.bracketed_paste(),
            hyperlinks: caps.hyperlinks(),
            sixel: caps.sixel(),
            iterm2_image: caps.iterm2_image(),
            mouse_reporting: caps.mouse_reporting(),
            background_color_erase: caps.bce(),
        }
    }
}

pub(super) struct TerminalSession {
    pub terminal: TuiTerminal,
    pub capabilities: TerminalCapabilities,
}

pub(super) fn init_terminal() -> Result<TerminalSession> {
    let caps = terminal_capabilities()?;
    let capabilities = TerminalCapabilities::from_termwiz(&caps);
    let mut buffered_terminal = BufferedTerminal::new(SystemTerminal::new(caps)?)?;
    buffered_terminal.terminal().set_raw_mode()?;
    buffered_terminal.terminal().enter_alternate_screen()?;
    let backend = TermwizBackend::with_buffered_terminal(buffered_terminal);
    Ok(TerminalSession {
        terminal: Terminal::new(backend)?,
        capabilities,
    })
}

pub(super) fn init_input_terminal() -> Result<TuiInputTerminal> {
    Ok(SystemTerminal::new(terminal_capabilities()?)?)
}

pub(super) fn read_input_event(terminal: &mut TuiInputTerminal) -> Result<Option<InputEvent>> {
    Ok(terminal.poll_input(None)?)
}

fn terminal_capabilities() -> Result<Capabilities> {
    let hints =
        ProbeHints::new_from_env().mouse_reporting(Some(mouse_capture_enabled_by_default()));
    Ok(Capabilities::new_with_hints(hints)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_capture_is_disabled_by_default_for_terminal_selection() {
        assert!(!mouse_capture_enabled_by_default());
    }

    #[test]
    fn capabilities_snapshot_maps_termwiz_values() {
        let caps = terminal_capabilities().expect("terminal caps from env");
        let snapshot = TerminalCapabilities::from_termwiz(&caps);
        assert_eq!(snapshot.bracketed_paste, caps.bracketed_paste());
        assert_eq!(snapshot.hyperlinks, caps.hyperlinks());
    }

    #[test]
    fn terminal_capabilities_keep_mouse_capture_disabled() {
        let caps = terminal_capabilities().expect("terminal caps from env");
        assert!(!caps.mouse_reporting());
    }
}
