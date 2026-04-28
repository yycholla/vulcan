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

    /// YYC-264: seed the cortex knowledge graph from the last N SQLite
    /// sessions on startup. Only applies when `[cortex].enabled = true`.
    /// Defaults to importing the 3 most recent sessions.
    #[arg(long, global = true)]
    pub seed_cortex: bool,
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
    /// Run the long-lived gateway daemon (axum server + platform
    /// connectors). Defaults to `run`; YYC-242 adds `init` for
    /// first-run config bootstrap.
    #[cfg(feature = "gateway")]
    Gateway {
        #[command(subcommand)]
        cmd: Option<GatewaySubcommand>,
        /// Override bind address from config (e.g. 127.0.0.1:7777).
        /// Equivalent to `vulcan gateway run --bind <addr>`.
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
    /// YYC-241: list + select models on the active provider.
    Model {
        #[command(subcommand)]
        cmd: ModelSubcommand,
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
    /// YYC-219: inspect named context packs (curated bundles of
    /// project files + notes used to brief the agent on a task
    /// area). `list` shows every pack; `show <name>` prints the
    /// resolved citation list.
    ContextPack {
        #[command(subcommand)]
        cmd: ContextPackSubcommand,
    },
    /// YYC-212: unified config CLI. List, get, and inspect every
    /// known config field by dotted path.
    Config {
        #[command(subcommand)]
        cmd: ConfigSubcommand,
    },
    /// YYC-182: inspect workspace trust resolution.
    Trust {
        #[command(subcommand)]
        cmd: TrustSubcommand,
    },
    /// YYC-190: bounded critic pass on a plan, diff, run id, or
    /// arbitrary text. Runs read-only (`reviewer` profile).
    Review {
        #[command(subcommand)]
        cmd: ReviewSubcommand,
    },
    /// YYC-183: structured runtime diagnostics — config,
    /// storage, workspace, tool registry checks.
    Doctor,
    /// YYC-221: render a release summary for a git revision range
    /// (e.g. `main..HEAD`). Walks `git log`, groups commits by
    /// `YYC-<id>` issue refs, surfaces risk-flagged subjects, and
    /// prints markdown to stdout.
    Release {
        /// Git revision range. Anything `git log` accepts works.
        range: String,
    },
    /// YYC-185: dry-run effective tool policy for a workspace
    /// + capability profile combination, without executing
    /// anything.
    Policy {
        #[command(subcommand)]
        cmd: PolicySubcommand,
    },
    /// YYC-184: inspect or replay saved agent runs.
    Replay {
        #[command(subcommand)]
        cmd: ReplaySubcommand,
    },
    /// YYC-218 / YYC-189: generate a change-impact report for a
    /// file. Walks code references + tests + docs and emits
    /// markdown.
    Impact {
        /// File path to analyze.
        target: std::path::PathBuf,
        /// Persist the rendered report as a YYC-180 artifact.
        #[arg(long)]
        save: bool,
    },
    /// YYC-220 / YYC-187: project playbook management.
    Playbook {
        #[command(subcommand)]
        cmd: PlaybookSubcommand,
    },
    /// YYC-167: extension lifecycle CLI. Settings live under
    /// `vulcan config extensions` (YYC-212).
    Extension {
        #[command(subcommand)]
        cmd: ExtensionSubcommand,
    },
    /// YYC-264: inspect and manage the embedded Cortex knowledge graph.
    /// Direct access to store facts, semantic search, graph stats, and
    /// seed from SQLite sessions.
    Cortex {
        #[command(subcommand)]
        cmd: CortexSubcommand,
    },
}

/// YYC-242 subcommands under `vulcan gateway`. `Run` is the
/// default when the user just types `vulcan gateway`.
#[cfg(feature = "gateway")]
#[derive(Subcommand, Debug)]
pub enum GatewaySubcommand {
    /// Run the long-lived daemon (default).
    Run {
        /// Override bind address from config (e.g. 127.0.0.1:7777).
        #[arg(long)]
        bind: Option<String>,
    },
    /// Bootstrap `[gateway]` config + a fresh `api_token` on
    /// first run.
    Init {
        /// Overwrite an existing `[gateway]` section.
        #[arg(long)]
        force: bool,
    },
}

/// YYC-241 subcommands under `vulcan model`.
#[derive(Subcommand, Debug)]
pub enum ModelSubcommand {
    /// Query the active provider's `/models` catalog.
    List,
    /// Show the currently-active provider + model.
    Show,
    /// Persist a new `model = "<id>"` on the active provider.
    Use {
        id: String,
        /// Skip catalog membership validation (useful for
        /// self-hosted endpoints with no `/models` endpoint).
        #[arg(long)]
        force: bool,
    },
}

/// YYC-167 subcommands.
#[derive(Subcommand, Debug)]
pub enum ExtensionSubcommand {
    /// List installed extensions with status + last load error.
    List,
    /// Show full metadata + install state for one extension.
    Show { id: String },
    /// Promote install state to `enabled = true`.
    Enable { id: String },
    /// Demote install state to `enabled = false`.
    Disable { id: String },
    /// Permanently delete the install directory + state row.
    Uninstall {
        id: String,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Scaffold a new extension directory in `<cwd>/<name>/`.
    New {
        name: String,
        /// Skeleton kind. `prompt` = builtin manifest only;
        /// `rust` = builtin manifest + Cargo project stub.
        #[arg(long, default_value = "prompt")]
        kind: String,
    },
    /// Parse + verify a manifest at the given path without
    /// touching the install state.
    Validate { path: std::path::PathBuf },
    /// Copy a manifest directory into the Vulcan home and
    /// register an install state row.
    Install { path: std::path::PathBuf },
}

/// YYC-264: subcommands under `vulcan cortex`.
#[derive(Subcommand, Debug)]
pub enum CortexSubcommand {
    /// Store a fact node in the cortex knowledge graph.
    Store {
        /// Fact text to store.
        text: String,
        /// Importance from 0.0 to 1.0 (default 0.7).
        #[arg(long, default_value_t = 0.7)]
        importance: f32,
    },
    /// Semantic vector search across the knowledge graph.
    Search {
        /// Natural language query.
        query: String,
        /// Max results to return (default 5).
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },
    /// Show graph statistics (node count, edge count, db size).
    Stats,
    /// Seed the cortex graph from recent SQLite sessions.
    Seed {
        /// Number of most recent sessions to import (default 3).
        #[arg(long, default_value_t = 3)]
        sessions: usize,
    },
    /// List recently stored facts in the graph.
    Recall {
        /// Max entries to show (default 20).
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

#[cfg(all(test, feature = "gateway"))]
mod tests {
    use super::*;

    #[test]
    fn gateway_run_accepts_bind_on_run_subcommand() {
        let cli = Cli::parse_from(["vulcan", "gateway", "run", "--bind", "127.0.0.1:0"]);

        match cli.command {
            Some(Command::Gateway {
                cmd: Some(GatewaySubcommand::Run { bind }),
                bind: parent_bind,
            }) => {
                assert_eq!(bind.as_deref(), Some("127.0.0.1:0"));
                assert!(parent_bind.is_none());
            }
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn gateway_legacy_bind_form_still_defaults_to_run() {
        let cli = Cli::parse_from(["vulcan", "gateway", "--bind", "127.0.0.1:0"]);

        match cli.command {
            Some(Command::Gateway { cmd, bind }) => {
                assert!(cmd.is_none());
                assert_eq!(bind.as_deref(), Some("127.0.0.1:0"));
            }
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn gateway_init_has_no_bind_option() {
        let err = Cli::try_parse_from(["vulcan", "gateway", "init", "--bind", "127.0.0.1:0"])
            .expect_err("init should not accept --bind");

        assert_eq!(err.kind(), clap::error::ErrorKind::UnknownArgument);
    }
}

/// YYC-220: subcommands under `vulcan playbook`.
#[derive(Subcommand, Debug)]
pub enum PlaybookSubcommand {
    /// List entries for the current workspace.
    List {
        /// Show only entries with a specific status (`proposed` /
        /// `accepted`).
        #[arg(long)]
        status: Option<String>,
    },
    /// Print full body for a single entry id.
    Show { id: String },
    /// Mark a `Proposed` entry as `Accepted`.
    Accept { id: String },
    /// Delete an entry permanently.
    Remove { id: String },
    /// Import `AGENTS.md` / `CLAUDE.md` / `README.md` from the
    /// workspace root as `Proposed` entries.
    Import {
        /// Workspace root (defaults to current directory).
        #[arg(long)]
        path: Option<std::path::PathBuf>,
    },
}

/// YYC-184: subcommands under `vulcan replay`.
#[derive(Subcommand, Debug)]
pub enum ReplaySubcommand {
    /// Print the saved timeline for a run id (UUID or 8-char
    /// prefix). Read-only — no re-execution.
    Inspect { id: String },
}

/// YYC-185: subcommands under `vulcan policy`.
#[derive(Subcommand, Debug)]
pub enum PolicySubcommand {
    /// Resolve effective policy for a workspace path. Defaults
    /// to the current working directory.
    Simulate {
        path: Option<std::path::PathBuf>,
        /// Optional profile override; defaults to whatever the
        /// agent would resolve at session start.
        #[arg(long)]
        profile: Option<String>,
    },
}

/// YYC-190: subcommands under `vulcan review`.
#[derive(Subcommand, Debug)]
pub enum ReviewSubcommand {
    /// Critique an implementation plan supplied as a path or `-`
    /// for stdin.
    Plan {
        /// File path containing the plan, or `-` for stdin.
        target: String,
    },
    /// Critique a diff supplied as a path or `-` for stdin.
    Diff {
        /// File path containing the diff, or `-` for stdin.
        target: String,
    },
    /// Critique a past run by id (UUID or 8-char prefix).
    Run { id: String },
}

/// YYC-182: subcommands under `vulcan trust`.
#[derive(Subcommand, Debug)]
pub enum TrustSubcommand {
    /// Explain why a workspace path resolved to its current trust
    /// profile. Defaults to the current working directory when no
    /// path is given.
    Why { path: Option<std::path::PathBuf> },
}

/// YYC-212: subcommands under `vulcan config`. PR-1 shipped the
/// read-only paths; PR-2 adds `set` / `unset` writers; future PRs
/// land `edit` (interactive) and the `auth`/`provider`/`skills`
/// nested namespaces.
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
    /// Validate a value against the field's declared kind, then
    /// write it to the right TOML file with comments preserved.
    Set {
        /// Dotted field path.
        key: String,
        /// New value. Bools accept `true|false|on|off|yes|no`;
        /// ints parse as base 10; enums must match a declared
        /// variant exactly.
        value: String,
    },
    /// Remove a field's override from disk. Subsequent reads fall
    /// back to the declared default.
    Unset {
        /// Dotted field path.
        key: String,
    },
    /// YYC-217: open the config file containing a given section in
    /// `$EDITOR` for ad-hoc edits. Prints the path when no editor
    /// is set so the user can open it themselves.
    Edit {
        /// Optional section name (e.g. `provider`, `gateway`,
        /// `keybinds`). Routes to the right TOML file across the
        /// split layout (`config.toml` / `keybinds.toml` /
        /// `providers.toml`). Omit to edit `config.toml`.
        section: Option<String>,
    },
}

/// YYC-219: subcommands under `vulcan context-pack`.
#[derive(Subcommand, Debug)]
pub enum ContextPackSubcommand {
    /// List every available context pack (built-in + user-defined).
    List,
    /// Render a single pack's citation list to stdout.
    Show {
        /// Pack name. Case-insensitive match against the catalog.
        name: String,
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
