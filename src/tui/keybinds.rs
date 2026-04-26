//! Configurable key bindings for the TUI (YYC-90).
//!
//! Owns the parser that turns user-facing strings like `"Ctrl+K"` / `"⌃K"` /
//! `"F2"` / `"Esc"` into a `KeyBinding`, and the bag (`Keybinds`) the input
//! handler matches against and the prompt-row reads its hint labels from.

use std::str::FromStr;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::KeybindsConfig;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyBinding {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl KeyBinding {
    pub fn matches(&self, ev: &KeyEvent) -> bool {
        ev.code == self.code && ev.modifiers == self.mods
    }

    /// Caret-prefixed label suited for prompt-row footer (e.g. `⌃K`, `F2`,
    /// `Esc`). Stable regardless of how the user spelled it in config.
    pub fn label(&self) -> String {
        let mut out = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            out.push('⌃');
        }
        if self.mods.contains(KeyModifiers::ALT) {
            out.push('⌥');
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            out.push('⇧');
        }
        match self.code {
            KeyCode::Char(c) => out.push(c.to_ascii_uppercase()),
            KeyCode::F(n) => out.push_str(&format!("F{n}")),
            KeyCode::Esc => out.push_str("Esc"),
            KeyCode::Enter => out.push('↵'),
            KeyCode::Tab => out.push_str("Tab"),
            KeyCode::Backspace => out.push('⌫'),
            KeyCode::Up => out.push('↑'),
            KeyCode::Down => out.push('↓'),
            KeyCode::Left => out.push('←'),
            KeyCode::Right => out.push('→'),
            other => out.push_str(&format!("{other:?}")),
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct KeyParseError(pub String);

impl std::fmt::Display for KeyParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unrecognised key binding: {}", self.0)
    }
}

impl std::error::Error for KeyParseError {}

impl FromStr for KeyBinding {
    type Err = KeyParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(KeyParseError(s.to_string()));
        }

        let mut mods = KeyModifiers::NONE;
        let mut rest = trimmed.to_string();

        loop {
            // Caret-style sigils first (⌃, ⌥, ⇧).
            let mut consumed = false;
            for (sigil, modifier) in [
                ('⌃', KeyModifiers::CONTROL),
                ('⌥', KeyModifiers::ALT),
                ('⇧', KeyModifiers::SHIFT),
            ] {
                if let Some(stripped) = rest.strip_prefix(sigil) {
                    mods.insert(modifier);
                    rest = stripped.to_string();
                    consumed = true;
                    break;
                }
            }
            if consumed {
                continue;
            }

            // `Ctrl+`, `Alt+`, `Shift+` prefixes (case-insensitive).
            let lower = rest.to_ascii_lowercase();
            if let Some(after) = lower.strip_prefix("ctrl+") {
                mods.insert(KeyModifiers::CONTROL);
                rest = rest[rest.len() - after.len()..].to_string();
                continue;
            }
            if let Some(after) = lower.strip_prefix("alt+") {
                mods.insert(KeyModifiers::ALT);
                rest = rest[rest.len() - after.len()..].to_string();
                continue;
            }
            if let Some(after) = lower.strip_prefix("shift+") {
                mods.insert(KeyModifiers::SHIFT);
                rest = rest[rest.len() - after.len()..].to_string();
                continue;
            }
            break;
        }

        if rest.is_empty() {
            return Err(KeyParseError(s.to_string()));
        }

        let code = match rest.as_str() {
            "Esc" | "esc" | "ESC" => KeyCode::Esc,
            "Enter" | "enter" | "Return" | "return" | "↵" => KeyCode::Enter,
            "Tab" | "tab" => KeyCode::Tab,
            "Backspace" | "backspace" | "⌫" => KeyCode::Backspace,
            "Up" | "up" | "↑" => KeyCode::Up,
            "Down" | "down" | "↓" => KeyCode::Down,
            "Left" | "left" | "←" => KeyCode::Left,
            "Right" | "right" | "→" => KeyCode::Right,
            other => {
                if let Some(n) = other
                    .strip_prefix('F')
                    .or_else(|| other.strip_prefix('f'))
                    .and_then(|d| d.parse::<u8>().ok())
                {
                    if (1..=24).contains(&n) {
                        KeyCode::F(n)
                    } else {
                        return Err(KeyParseError(s.to_string()));
                    }
                } else {
                    let mut chars = other.chars();
                    let first = chars.next().ok_or_else(|| KeyParseError(s.to_string()))?;
                    if chars.next().is_some() {
                        return Err(KeyParseError(s.to_string()));
                    }
                    // Ctrl-shorthand keys are conventionally lowercased.
                    let ch = if mods.contains(KeyModifiers::CONTROL) {
                        first.to_ascii_lowercase()
                    } else {
                        first
                    };
                    KeyCode::Char(ch)
                }
            }
        };

        Ok(Self { code, mods })
    }
}

/// Parsed binding bag — built once at TUI startup from `KeybindsConfig`.
#[derive(Clone, Debug)]
pub struct Keybinds {
    pub toggle_sessions: KeyBinding,
    pub toggle_tools: KeyBinding,
    pub toggle_reasoning: KeyBinding,
    pub cancel: KeyBinding,
    pub queue_drop: KeyBinding,
}

impl Keybinds {
    /// Build from config strings, falling back to the in-code default if a
    /// user-supplied value fails to parse. Reports each fallback via tracing
    /// so misconfigurations are visible.
    pub fn from_config(cfg: &KeybindsConfig) -> Self {
        let parse = |raw: &str, action: &str, fallback: KeyBinding| match raw.parse::<KeyBinding>() {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    "keybinds: failed to parse `{}` for `{action}` ({e}); using default {}",
                    raw,
                    fallback.label()
                );
                fallback
            }
        };

        Self {
            toggle_sessions: parse(
                &cfg.toggle_sessions,
                "toggle_sessions",
                Self::defaults().toggle_sessions,
            ),
            toggle_tools: parse(
                &cfg.toggle_tools,
                "toggle_tools",
                Self::defaults().toggle_tools,
            ),
            toggle_reasoning: parse(
                &cfg.toggle_reasoning,
                "toggle_reasoning",
                Self::defaults().toggle_reasoning,
            ),
            cancel: parse(&cfg.cancel, "cancel", Self::defaults().cancel),
            queue_drop: parse(&cfg.queue_drop, "queue_drop", Self::defaults().queue_drop),
        }
    }

    pub fn defaults() -> Self {
        Self {
            toggle_sessions: KeyBinding {
                code: KeyCode::Char('k'),
                mods: KeyModifiers::CONTROL,
            },
            toggle_tools: KeyBinding {
                code: KeyCode::Char('t'),
                mods: KeyModifiers::CONTROL,
            },
            toggle_reasoning: KeyBinding {
                code: KeyCode::Char('r'),
                mods: KeyModifiers::CONTROL,
            },
            cancel: KeyBinding {
                code: KeyCode::Char('c'),
                mods: KeyModifiers::CONTROL,
            },
            queue_drop: KeyBinding {
                code: KeyCode::Backspace,
                mods: KeyModifiers::CONTROL,
            },
        }
    }
}

impl Default for Keybinds {
    fn default() -> Self {
        Self::defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ctrl_plus_letter() {
        let b: KeyBinding = "Ctrl+K".parse().unwrap();
        assert_eq!(b.code, KeyCode::Char('k'));
        assert!(b.mods.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn parses_caret_sigil() {
        let b: KeyBinding = "⌃K".parse().unwrap();
        assert_eq!(b.code, KeyCode::Char('k'));
        assert!(b.mods.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn parses_function_key() {
        let b: KeyBinding = "F2".parse().unwrap();
        assert_eq!(b.code, KeyCode::F(2));
        assert!(b.mods.is_empty());
    }

    #[test]
    fn parses_named_keys() {
        assert_eq!("Esc".parse::<KeyBinding>().unwrap().code, KeyCode::Esc);
        assert_eq!("Tab".parse::<KeyBinding>().unwrap().code, KeyCode::Tab);
        assert_eq!(
            "Backspace".parse::<KeyBinding>().unwrap().code,
            KeyCode::Backspace
        );
    }

    #[test]
    fn parses_ctrl_backspace() {
        let b: KeyBinding = "Ctrl+Backspace".parse().unwrap();
        assert_eq!(b.code, KeyCode::Backspace);
        assert!(b.mods.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn rejects_empty_and_garbage() {
        assert!("".parse::<KeyBinding>().is_err());
        assert!("Ctrl+".parse::<KeyBinding>().is_err());
        assert!("nonsense".parse::<KeyBinding>().is_err());
        assert!("F99".parse::<KeyBinding>().is_err());
    }

    #[test]
    fn label_uses_caret_for_ctrl_letters() {
        let b = KeyBinding {
            code: KeyCode::Char('k'),
            mods: KeyModifiers::CONTROL,
        };
        assert_eq!(b.label(), "⌃K");
    }

    #[test]
    fn label_function_and_esc() {
        let b = KeyBinding {
            code: KeyCode::F(2),
            mods: KeyModifiers::NONE,
        };
        assert_eq!(b.label(), "F2");
        let b = KeyBinding {
            code: KeyCode::Esc,
            mods: KeyModifiers::NONE,
        };
        assert_eq!(b.label(), "Esc");
    }

    #[test]
    fn matches_key_event() {
        let b: KeyBinding = "Ctrl+T".parse().unwrap();
        let ev = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert!(b.matches(&ev));
        let ev = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE);
        assert!(!b.matches(&ev));
    }

    #[test]
    fn keybinds_from_config_uses_defaults_for_unparseable() {
        let mut cfg = KeybindsConfig::default();
        cfg.toggle_tools = "garbage-string".into();
        let kb = Keybinds::from_config(&cfg);
        assert_eq!(kb.toggle_tools, Keybinds::defaults().toggle_tools);
    }

    #[test]
    fn keybinds_from_config_parses_overrides() {
        let mut cfg = KeybindsConfig::default();
        cfg.toggle_tools = "F2".into();
        let kb = Keybinds::from_config(&cfg);
        assert_eq!(kb.toggle_tools.code, KeyCode::F(2));
        assert!(kb.toggle_tools.mods.is_empty());
    }
}
