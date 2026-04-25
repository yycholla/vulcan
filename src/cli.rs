use clap::{Parser, Subcommand};

/// vulcan — a Rust AI agent. Forged at the forge, tested by fire.
#[derive(Parser, Debug)]
#[command(name = "vulcan", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Start interactive TUI session (default)
    Chat,
    /// Run a one-shot prompt and print the response
    Prompt {
        /// The prompt text to send to the agent
        text: String,
    },
    /// Resume a previous session by ID
    Session {
        /// Session ID to resume
        id: String,
    },
}
