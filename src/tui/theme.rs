use ratatui::style::{Color, Style};

/// Bauhaus / brutalist palette — cream paper, ink black, primary accents.
/// Matches the Rust AI agent TUI design (`Rust AI agent TUI.html`).
pub struct Palette;

impl Palette {
    pub const PAPER: Color = Color::Rgb(0xF2, 0xEE, 0xE5);
    pub const INK: Color = Color::Rgb(0x15, 0x13, 0x0F);
    pub const MUTED: Color = Color::Rgb(0x8A, 0x84, 0x78);
    pub const FAINT: Color = Color::Rgb(0xE2, 0xDC, 0xCD);
    /// Slate header bg for tool-card title bars (YYC-74). Lighter
    /// shade so the dark pill (`× name`) stands out against it; pairs
    /// with the body which sits on paper.
    pub const SLATE: Color = Color::Rgb(0xC8, 0xC2, 0xB5);
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

#[cfg(test)]
mod theme_tests {
    use super::*;

    #[test]
    fn from_name_system_returns_reset_bg() {
        let t = Theme::from_name("system");
        // system theme inherits terminal: body bg should be Reset.
        assert_eq!(t.body_bg, Color::Reset);
    }

    #[test]
    fn from_name_default_light_returns_paper_bg() {
        let t = Theme::from_name("default-light");
        // default-light formalizes today's Bauhaus palette.
        assert_eq!(t.body_bg, Palette::PAPER);
    }

    #[test]
    fn from_name_dracula_returns_dracula_bg() {
        let t = Theme::from_name("dracula");
        assert_eq!(t.body_bg, Color::Rgb(0x28, 0x2a, 0x36));
    }

    #[test]
    fn from_name_unknown_falls_back_to_system() {
        let unknown = Theme::from_name("nonexistent-theme");
        let system = Theme::from_name("system");
        assert_eq!(unknown.body_bg, system.body_bg);
    }
}
