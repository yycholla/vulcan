//! Terminal lifecycle helpers extracted from `tui/mod.rs` (YYC-108).
//!
//! `init_terminal` enables raw mode + alternate screen and hands back a
//! configured ratatui `Terminal`. `restore_terminal` undoes both on
//! shutdown so the user's shell isn't left in raw mode after a panic
//! or clean exit.

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

const ENABLE_MOUSE_CAPTURE_BY_DEFAULT: bool = false;

fn mouse_capture_enabled_by_default() -> bool {
    ENABLE_MOUSE_CAPTURE_BY_DEFAULT
}

pub(super) fn init_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    ratatui::crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    ratatui::crossterm::execute!(
        stdout,
        ratatui::crossterm::terminal::EnterAlternateScreen,
        // YYC-124: ask the terminal to wrap pastes in CSI 200~/201~ so
        // crossterm hands them to us as one Event::Paste(String) instead
        // of N KeyCode::Char events. Without this, multiline pastes
        // submit a prompt per line.
        ratatui::crossterm::event::EnableBracketedPaste,
    )?;
    if mouse_capture_enabled_by_default() {
        // YYC-581: mouse capture blocks normal terminal drag-selection in
        // common emulators. Keep it off by default so visible chat text can
        // be highlighted and copied; keyboard scrolling remains available.
        ratatui::crossterm::execute!(stdout, ratatui::crossterm::event::EnableMouseCapture)?;
    }
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub(super) fn restore_terminal() -> Result<()> {
    ratatui::crossterm::terminal::disable_raw_mode()?;
    ratatui::crossterm::execute!(
        std::io::stdout(),
        // Disable before leaving the alt screen so the user's primary
        // terminal isn't left in bracketed-paste / mouse-capture state
        // if Vulcan exits. This is intentionally defensive even though
        // mouse capture is not enabled by default.
        ratatui::crossterm::event::DisableMouseCapture,
        ratatui::crossterm::event::DisableBracketedPaste,
        ratatui::crossterm::terminal::LeaveAlternateScreen,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_capture_is_disabled_by_default_for_terminal_selection() {
        assert!(!mouse_capture_enabled_by_default());
    }
}
