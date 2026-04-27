//! Slash-command table + key/palette helpers extracted from
//! `tui/mod.rs` (YYC-108).
//!
//! Owns the `SlashCommand` struct, the `SLASH_COMMANDS` const table,
//! prefix-match + completion helpers, and the model/provider list
//! formatters (kept around for `--help`-style printers and tests).

use crate::config::Config;

#[derive(Debug, Clone)]
pub(super) struct SlashCommand {
    pub(super) name: &'static str,
    pub(super) description: &'static str,
    /// True when the command can run mid-turn without corrupting agent state
    /// (YYC-62). Pure UI ops are safe; anything that mutates conversation
    /// history or reaches into the agent is not. Default false (conservative).
    pub(super) mid_turn_safe: bool,
}

pub(super) const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "exit",
        description: "Quit Vulcan",
        // Always exits cleanly; no state to corrupt.
        mid_turn_safe: true,
    },
    SlashCommand {
        name: "quit",
        description: "Quit Vulcan",
        mid_turn_safe: true,
    },
    SlashCommand {
        name: "help",
        description: "Show available commands",
        // Pure UI: pushes a system message.
        mid_turn_safe: true,
    },
    SlashCommand {
        name: "clear",
        description: "Clear message history",
        // Destructive: would nuke the in-flight User+Agent pair the agent
        // loop is streaming into. Defer until idle.
        mid_turn_safe: false,
    },
    SlashCommand {
        name: "view",
        description: "Cycle to next view (or 1-5)",
        mid_turn_safe: true,
    },
    SlashCommand {
        name: "reasoning",
        description: "Toggle reasoning trace",
        mid_turn_safe: true,
    },
    SlashCommand {
        name: "search",
        description: "Search past sessions: /search <query>",
        // Holds agent.lock().await — would deadlock against the in-flight
        // run_prompt_stream task. Defer until idle.
        mid_turn_safe: false,
    },
    SlashCommand {
        name: "model",
        description: "List or switch models: /model [id]",
        // Rebuilds the provider for future turns and may fetch the catalog.
        // Defer until idle so the in-flight provider stream is untouched.
        mid_turn_safe: false,
    },
    SlashCommand {
        name: "provider",
        description: "List or switch named providers: /provider [name|default]",
        // Rebuilds the provider against a different profile; same idle
        // discipline as /model.
        mid_turn_safe: false,
    },
    SlashCommand {
        name: "skills",
        description: "List loaded skills",
        mid_turn_safe: true,
    },
    SlashCommand {
        name: "status",
        description: "Show session token usage + active provider",
        mid_turn_safe: true,
    },
    SlashCommand {
        name: "queue",
        description: "Queue prompt for after current turn: /queue <text>",
        mid_turn_safe: true,
    },
    SlashCommand {
        name: "resume",
        description: "Open session picker to switch to another session",
        mid_turn_safe: false,
    },
];

#[allow(dead_code)] // retained for tests and potential `--help`-style printers.
pub(super) fn format_model_list(
    active_model: &str,
    models: &[crate::provider::catalog::ModelInfo],
) -> String {
    let mut out = format!("Models from active provider ({} total):", models.len());
    for model in models.iter().take(30) {
        let marker = if model.id == active_model { "*" } else { " " };
        let context = if model.context_length > 0 {
            crate::tui::state::format_thousands(model.context_length as u32)
        } else {
            "unknown".into()
        };
        let mut flags = Vec::new();
        if model.features.tools {
            flags.push("tools");
        }
        if model.features.reasoning {
            flags.push("reasoning");
        }
        if model.features.vision {
            flags.push("vision");
        }
        if model.features.json_mode {
            flags.push("json");
        }
        let flags = if flags.is_empty() {
            String::new()
        } else {
            format!(" · {}", flags.join(","))
        };
        out.push_str(&format!("\n  {marker} {} · ctx {context}{flags}", model.id));
    }
    if models.len() > 30 {
        out.push_str(&format!("\n  ... {} more", models.len() - 30));
    }
    out.push_str("\n\nUse /model <id> to switch.");
    out
}

pub(super) fn build_provider_picker_entries(
    config: &Config,
) -> Vec<crate::tui::state::ProviderPickerEntry> {
    use crate::tui::state::ProviderPickerEntry;
    let mut out = Vec::with_capacity(config.providers.len() + 1);
    out.push(ProviderPickerEntry {
        name: None,
        model: config.provider.model.clone(),
        base_url: config.provider.base_url.clone(),
    });
    let mut names: Vec<&String> = config.providers.keys().collect();
    names.sort();
    for name in names {
        let cfg = &config.providers[name];
        out.push(ProviderPickerEntry {
            name: Some(name.clone()),
            model: cfg.model.clone(),
            base_url: cfg.base_url.clone(),
        });
    }
    out
}

#[allow(dead_code)] // retained for tests and potential `--help`-style printers.
pub(super) fn format_provider_list(config: &Config, active: Option<&str>) -> String {
    let mut out = String::from("Provider profiles:");
    let default_marker = if active.is_none() { "*" } else { " " };
    out.push_str(&format!(
        "\n  {default_marker} default · {} · {}",
        config.provider.base_url, config.provider.model,
    ));

    let mut names: Vec<&String> = config.providers.keys().collect();
    names.sort();
    for name in names {
        let cfg = &config.providers[name];
        let marker = if active == Some(name.as_str()) {
            "*"
        } else {
            " "
        };
        out.push_str(&format!(
            "\n  {marker} {name} · {} · {}",
            cfg.base_url, cfg.model,
        ));
    }
    if config.providers.is_empty() {
        out.push_str("\n  (no named [providers.<name>] profiles configured)");
    }
    out.push_str("\n\nUse /provider <name> to switch, /provider default to revert.");
    out
}

pub(super) fn filter_commands(prefix: &str) -> Vec<&'static SlashCommand> {
    if prefix.is_empty() {
        return SLASH_COMMANDS.iter().collect();
    }
    let lower = prefix.to_lowercase();
    SLASH_COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(&lower))
        .collect()
}

/// Same matching logic as the palette renderer in the main loop — exposed
/// as a helper so the key handler can decide what to highlight or commit
/// without duplicating prefix logic (YYC-70).
pub(super) fn current_palette(input: &str) -> Vec<&'static SlashCommand> {
    if input == "/" {
        SLASH_COMMANDS.iter().collect()
    } else if input.starts_with('/') && input.len() > 1 {
        filter_commands(&input[1..])
    } else {
        Vec::new()
    }
}

pub(super) fn complete_slash(prefix: &str) -> Option<String> {
    let matches = filter_commands(prefix);
    if matches.is_empty() {
        return None;
    }
    if matches.len() == 1 {
        return Some(matches[0].name.to_string());
    }
    let first = matches[0].name.as_bytes();
    let mut common = first.len();
    for m in &matches[1..] {
        let bytes = m.name.as_bytes();
        common = common.min(bytes.len());
        for (i, &b) in first.iter().enumerate().take(common) {
            if b != bytes[i] {
                common = i;
                break;
            }
        }
    }
    if common > prefix.len() {
        Some(matches[0].name[..common].to_string())
    } else {
        None
    }
}
