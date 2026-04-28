use clap::{Parser, Subcommand};
use clap_complete::Shell;

use crate::cli_auth::AuthArgs;
use crate::cli_provider::ProviderCommand;

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

    /// YYC-181: start the session under a named tool capability
    /// profile (e.g. `readonly`, `coding`, `reviewer`,
    /// `gateway-safe`). Overrides `tools.profile` from config.
    #[arg(long, global = true, value_name = "NAME")]
    pub profile: Option<String>,
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
    /// Split monolithic ~/.vulcan/config.toml into config + keybinds +
    /// providers fragment files (YYC-99). Idempotent. Existing fragment
    /// files are preserved unless `--force` is passed.
    MigrateConfig {
        #[arg(long)]
        force: bool,
    },
    /// Manage named provider profiles in ~/.vulcan/providers.toml (YYC-98).
    Provider {
        #[command(subcommand)]
        cmd: ProviderCommand,
    },
    /// Guided interactive provider setup (YYC-100). Picker + prompts for
    /// name, API key, and default model; writes to providers.toml.
    Auth(AuthArgs),
    /// YYC-213: print a shell-completion script. Source the output
    /// to enable tab completion for subcommands and global flags.
    /// Example: `vulcan completions bash > /etc/bash_completion.d/vulcan`.
    Completions {
        /// Target shell (bash, zsh, fish, powershell, elvish).
        shell: Shell,
    },
    /// YYC-179: inspect durable run records persisted by past
    /// agent turns. `vulcan run list` shows recent turns;
    /// `vulcan run show <id>` prints the full timeline.
    Run {
        #[command(subcommand)]
        cmd: RunSubcommand,
    },
    /// YYC-180: inspect typed artifacts (plans, diffs, reports,
    /// subagent summaries) persisted alongside agent turns.
    Artifact {
        #[command(subcommand)]
        cmd: ArtifactSubcommand,
    },
    /// YYC-194: governance + purge controls for local knowledge
    /// indexes (code graph, embeddings, sessions, run records,
    /// artifacts).
    Knowledge {
        #[command(subcommand)]
        cmd: KnowledgeSubcommand,
    },
    /// YYC-212: unified config CLI. List, get, and inspect every
    /// known config field by dotted path.
    Config {
        #[command(subcommand)]
        cmd: ConfigSubcommand,
    },
}

/// YYC-212: subcommands under `vulcan config`. PR-1 implements
/// the read-only paths; `set`/`unset`/`edit` land in follow-ups.
#[derive(Subcommand, Debug)]
pub enum ConfigSubcommand {
    /// List every declared config field with its kind, default,
    /// and target file.
    List,
    /// Print the resolved value for a single dotted key (e.g.
    /// `tools.native_enforcement`). Falls back to the declared
    /// default when the field is unset.
    Get {
        /// Dotted field path.
        key: String,
        /// Reveal secret values instead of redacting.
        #[arg(long)]
        reveal: bool,
    },
    /// Print the absolute paths of the config files Vulcan reads
    /// from. Honors the `~/.vulcan/` split (config/keybinds/
    /// providers).
    Path,
    /// Dump the merged in-memory config (defaults + files + env).
    /// Secret fields print redacted unless `--reveal`.
    Show {
        #[arg(long)]
        reveal: bool,
    },
}

/// YYC-194: subcommands under `vulcan knowledge`.
#[derive(Subcommand, Debug)]
pub enum KnowledgeSubcommand {
    /// List all discovered local knowledge stores with size +
    /// last-modified time.
    List,
    /// Permanently delete one or more local knowledge stores.
    /// Asks for confirmation unless `--yes` is set.
    Purge {
        /// Index kind to purge. Required for safety — purging all
        /// stores is a separate `--all` opt-in.
        #[arg(long)]
        kind: Option<String>,
        /// Workspace key (filename stem) when targeting a single
        /// per-workspace store.
        #[arg(long)]
        workspace: Option<String>,
        /// Purge every discovered store regardless of kind.
        #[arg(long, conflicts_with_all = ["kind", "workspace"])]
        all: bool,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
}

/// YYC-180: subcommands under `vulcan artifact`.
#[derive(Subcommand, Debug)]
pub enum ArtifactSubcommand {
    /// List recent artifacts (newest first).
    List {
        /// Maximum number of artifacts to display (default 20).
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Optional run id (UUID or 8-char prefix) to filter by.
        #[arg(long)]
        run: Option<String>,
        /// Optional session id to filter by.
        #[arg(long)]
        session: Option<String>,
    },
    /// Print full content + metadata for a single artifact id.
    Show {
        /// Artifact id (UUID, full or 8-char prefix).
        id: String,
    },
}

/// YYC-179: subcommands under `vulcan run`.
#[derive(Subcommand, Debug)]
pub enum RunSubcommand {
    /// List the most recent run records.
    List {
        /// Maximum number of runs to display (default 20).
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Print the full event timeline for a single run.
    Show {
        /// Run id (UUID, full or 8-char prefix).
        id: String,
    },
}
