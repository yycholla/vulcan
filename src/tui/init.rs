//! Terminal lifecycle helpers extracted from `tui/mod.rs` (YYC-108).
//! Owns the raw-mode + alternate-screen toggles so `run_tui` can stay
//! focused on the agent / event loop.

use anyhow::Result;
use ratatui::Terminal;

pub(super) fn init_terminal()
-> Result<Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>> {
    ratatui::crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    ratatui::crossterm::execute!(stdout, ratatui::crossterm::terminal::EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub(super) fn restore_terminal() -> Result<()> {
    let _ = ratatui::crossterm::terminal::disable_raw_mode();
    ratatui::crossterm::execute!(
        std::io::stdout(),
        ratatui::crossterm::terminal::LeaveAlternateScreen,
    )?;
    Ok(())
}
