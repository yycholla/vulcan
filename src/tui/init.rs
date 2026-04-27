//! Terminal lifecycle helpers extracted from `tui/mod.rs` (YYC-108).
//!
//! `init_terminal` enables raw mode + alternate screen and hands back a
//! configured ratatui `Terminal`. `restore_terminal` undoes both on
//! shutdown so the user's shell isn't left in raw mode after a panic
//! or clean exit.

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub(super) fn init_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    ratatui::crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    ratatui::crossterm::execute!(stdout, ratatui::crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
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
