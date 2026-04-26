use clap::{Parser, Subcommand};

/// vulcan — a Rust AI agent. Forged at the forge, tested by fire.
#[derive(Parser, Debug)]
#[command(name = "vulcan", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Resume the most recent session. Applies to `chat` (default) and
    /// `prompt` subcommands. Ignored for `session` (which already targets a
    /// specific ID) and `search`.
    #[arg(long, global = true)]
    pub r#continue: bool,

    /// Open the TUI with a session picker to choose which session to resume.
    /// Lists recent sessions — arrow-key to select, Enter to resume.
    #[arg(long, global = true)]
    pub resume: bool,
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
    /// Resume a previous session by ID (interactive TUI)
    Session {
        /// Session ID to resume
        id: String,
    },
    /// Full-text search across all saved sessions
    Search {
        /// FTS5 query (matches against message content)
        query: String,
        /// Max results to return
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Run the long-lived gateway daemon (axum server + platform connectors)
    #[cfg(feature = "gateway")]
    Gateway {
        /// Override bind address from config (e.g. 127.0.0.1:7777)
        #[arg(long)]
        bind: Option<String>,
    },
}
