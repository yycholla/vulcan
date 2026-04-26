use ratatui::style::{Color, Style};

/// Bauhaus / brutalist palette — cream paper, ink black, primary accents.
/// Matches the Rust AI agent TUI design (`Rust AI agent TUI.html`).
pub struct Palette;

impl Palette {
    pub const PAPER: Color = Color::Rgb(0xF2, 0xEE, 0xE5);
    pub const INK: Color = Color::Rgb(0x15, 0x13, 0x0F);
    pub const MUTED: Color = Color::Rgb(0x8A, 0x84, 0x78);
    pub const FAINT: Color = Color::Rgb(0xE2, 0xDC, 0xCD);
    /// Slate header bg for tool-card title bars (YYC-74). Dark enough
    /// to clearly separate from the FAINT body band but lighter than
    /// INK so the pills (which use INK + paper) still pop.
    pub const SLATE: Color = Color::Rgb(0x4A, 0x44, 0x38);
    pub const RED: Color = Color::Rgb(0xD6, 0x3B, 0x2F);
    pub const YELLOW: Color = Color::Rgb(0xE8, 0xB4, 0x3C);
    pub const BLUE: Color = Color::Rgb(0x2B, 0x4F, 0xA8);
    pub const GREEN: Color = Color::Rgb(0x3F, 0x7A, 0x4F);
}

/// Default body style — ink on paper.
pub fn body() -> Style {
    Style::default().fg(Palette::INK).bg(Palette::PAPER)
}

/// Inverse — paper on ink (used for header bars, ticker, mode pill).
pub fn inverse() -> Style {
    Style::default().fg(Palette::PAPER).bg(Palette::INK)
}

/// Muted body text.
pub fn muted() -> Style {
    Style::default().fg(Palette::MUTED).bg(Palette::PAPER)
}

/// Faint backdrop — used for inactive rails and reasoning trace.
pub fn faint_bg() -> Style {
    Style::default().fg(Palette::INK).bg(Palette::FAINT)
}
