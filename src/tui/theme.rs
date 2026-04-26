use ratatui::style::{Color, Modifier, Style};

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

/// Default body style — terminal-default background.
pub fn body() -> Style {
    Style::default().fg(Palette::INK)
}

/// Bold emphasis for header bars and chrome. No background paint so
/// the active terminal theme shows through.
pub fn inverse() -> Style {
    Style::default().fg(Palette::INK).add_modifier(Modifier::BOLD)
}

/// Muted body text.
pub fn muted() -> Style {
    Style::default().fg(Palette::MUTED)
}

/// Inset trace style (reasoning, side rails).
pub fn faint_bg() -> Style {
    Style::default().fg(Palette::MUTED).add_modifier(Modifier::ITALIC)
}

/// Centralized style table — one entry per theming role.
///
/// `body_bg`/`body_fg` are referenced by render code that needs the
/// background color of the chat surface (card fills, inverse-region
/// inverses) and aren't "roles" in the chat/markdown sense — but
/// keeping them on `Theme` lets render code avoid reaching back into
/// `Palette::*` constants when the active theme is non-Bauhaus.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    // chat
    pub user: Style,
    pub assistant: Style,
    pub system: Style,
    pub tool_call: Style,
    pub tool_result: Style,
    pub error: Style,
    pub success: Style,
    pub muted: Style,
    pub accent: Style,
    pub border: Style,

    // markdown
    pub heading_1: Style,
    pub heading_2: Style,
    pub heading_3: Style,
    pub heading_4: Style,
    pub heading_5: Style,
    pub heading_6: Style,
    pub code_block: Style,
    pub inline_code: Style,
    pub link: Style,
    pub blockquote: Style,
    pub list_marker: Style,
    pub strikethrough: Style,

    // structural
    pub body_bg: Color,
    pub body_fg: Color,
}

impl Theme {
    pub fn from_name(name: &str) -> Self {
        match name {
            "default-light" => Self::default_light(),
            "dracula" => Self::dracula(),
            "system" => Self::system(),
            other => {
                tracing::warn!(
                    "unknown theme '{other}', falling back to 'system'"
                );
                Self::system()
            }
        }
    }

    /// Inherits the user's terminal palette: `Color::Reset` for unstyled
    /// roles, ANSI-named slots (Cyan/Yellow/Red/etc.) for emphasis. Works
    /// on every terminal; readable on light + dark schemes.
    pub fn system() -> Self {
        let plain = Style::default();
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let italic = Style::default().add_modifier(Modifier::ITALIC);
        Self {
            user: plain,
            assistant: plain,
            system: Style::default().fg(Color::Cyan),
            tool_call: Style::default().fg(Color::Yellow),
            tool_result: Style::default().fg(Color::Green),
            error: Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            success: Style::default().fg(Color::Green),
            muted: Style::default().fg(Color::DarkGray),
            accent: Style::default().fg(Color::Magenta),
            border: Style::default().fg(Color::DarkGray),

            heading_1: bold,
            heading_2: bold,
            heading_3: bold,
            heading_4: bold,
            heading_5: bold,
            heading_6: bold,
            code_block: Style::default(),
            inline_code: Style::default().fg(Color::Cyan),
            link: Style::default().fg(Color::Blue).add_modifier(Modifier::UNDERLINED),
            blockquote: italic,
            list_marker: Style::default().fg(Color::Yellow),
            strikethrough: Style::default().add_modifier(Modifier::CROSSED_OUT),

            body_bg: Color::Reset,
            body_fg: Color::Reset,
        }
    }

    /// Bauhaus / brutalist palette — ink text + colored accents on the
    /// terminal's native background. Backgrounds always inherit so
    /// copy-paste behaves naturally.
    pub fn default_light() -> Self {
        let body = Style::default().fg(Palette::INK);
        let bold = body.add_modifier(Modifier::BOLD);
        let italic = body.add_modifier(Modifier::ITALIC);
        Self {
            user: body,
            assistant: body,
            system: Style::default().fg(Palette::BLUE),
            tool_call: Style::default().fg(Palette::YELLOW),
            tool_result: Style::default().fg(Palette::GREEN),
            error: Style::default().fg(Palette::RED).add_modifier(Modifier::BOLD),
            success: Style::default().fg(Palette::GREEN),
            muted: Style::default().fg(Palette::MUTED),
            accent: Style::default().fg(Palette::RED),
            border: Style::default().fg(Palette::MUTED),

            heading_1: bold,
            heading_2: bold,
            heading_3: bold,
            heading_4: bold,
            heading_5: bold,
            heading_6: bold,
            code_block: body,
            inline_code: Style::default().fg(Palette::RED),
            link: Style::default().fg(Palette::BLUE).add_modifier(Modifier::UNDERLINED),
            blockquote: italic,
            list_marker: Style::default().fg(Palette::BLUE),
            strikethrough: body.add_modifier(Modifier::CROSSED_OUT),

            body_bg: Color::Reset,
            body_fg: Palette::INK,
        }
    }

    /// Canonical Dracula foreground palette on the terminal's native
    /// background. Backgrounds always inherit so copy-paste behaves
    /// naturally.
    pub fn dracula() -> Self {
        const FG: Color = Color::Rgb(0xf8, 0xf8, 0xf2);
        const COMMENT: Color = Color::Rgb(0x62, 0x72, 0xa4);
        const CYAN: Color = Color::Rgb(0x8b, 0xe9, 0xfd);
        const GREEN: Color = Color::Rgb(0x50, 0xfa, 0x7b);
        const ORANGE: Color = Color::Rgb(0xff, 0xb8, 0x6c);
        const PINK: Color = Color::Rgb(0xff, 0x79, 0xc6);
        const PURPLE: Color = Color::Rgb(0xbd, 0x93, 0xf9);
        const RED: Color = Color::Rgb(0xff, 0x55, 0x55);
        const YELLOW: Color = Color::Rgb(0xf1, 0xfa, 0x8c);

        let body = Style::default().fg(FG);
        let bold_pink = Style::default().fg(PINK).add_modifier(Modifier::BOLD);
        Self {
            user: body,
            assistant: body,
            system: Style::default().fg(CYAN),
            tool_call: Style::default().fg(YELLOW),
            tool_result: Style::default().fg(GREEN),
            error: Style::default().fg(RED).add_modifier(Modifier::BOLD),
            success: Style::default().fg(GREEN),
            muted: Style::default().fg(COMMENT),
            accent: Style::default().fg(PURPLE),
            border: Style::default().fg(COMMENT),

            heading_1: bold_pink,
            heading_2: bold_pink,
            heading_3: bold_pink,
            heading_4: bold_pink,
            heading_5: bold_pink,
            heading_6: bold_pink,
            code_block: body,
            inline_code: Style::default().fg(ORANGE),
            link: Style::default().fg(CYAN).add_modifier(Modifier::UNDERLINED),
            blockquote: Style::default().fg(COMMENT).add_modifier(Modifier::ITALIC),
            list_marker: Style::default().fg(PURPLE),
            strikethrough: body.add_modifier(Modifier::CROSSED_OUT),

            body_bg: Color::Reset,
            body_fg: FG,
        }
    }
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
    fn all_themes_inherit_terminal_bg() {
        for name in ["system", "default-light", "dracula"] {
            let t = Theme::from_name(name);
            assert_eq!(
                t.body_bg,
                Color::Reset,
                "{name} must not paint a background"
            );
        }
    }

    #[test]
    fn from_name_unknown_falls_back_to_system() {
        let unknown = Theme::from_name("nonexistent-theme");
        let system = Theme::from_name("system");
        assert_eq!(unknown.body_bg, system.body_bg);
    }
}

#[cfg(test)]
mod theme_swap_tests {
    use super::*;

    // All themes share terminal-native body roles (assistant/user/code).
    // What differentiates them is their semantic accent palette — pin
    // those differences so a future palette change doesn't accidentally
    // collapse the themes onto identical accents.

    #[test]
    fn dracula_accent_differs_from_default_light() {
        let light = Theme::default_light();
        let drac = Theme::dracula();
        assert_ne!(light.accent, drac.accent);
    }

    #[test]
    fn dracula_inline_code_differs_from_default_light() {
        let light = Theme::default_light();
        let drac = Theme::dracula();
        assert_ne!(light.inline_code, drac.inline_code);
    }

    #[test]
    fn system_alone_inherits_terminal_body_fg() {
        let system = Theme::from_name("system");
        assert_eq!(system.body_fg, Color::Reset);
        assert_eq!(system.assistant.fg, None);
        // Painted themes intentionally stamp their own body fg.
        assert_eq!(Theme::from_name("default-light").body_fg, Palette::INK);
        assert_ne!(Theme::from_name("dracula").body_fg, Color::Reset);
    }

    #[test]
    fn system_assistant_inherits_terminal_fg() {
        // system theme must not paint over the terminal's default text color.
        let t = Theme::system();
        assert_eq!(t.assistant.fg, None);
    }
}
