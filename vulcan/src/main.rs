// GH issue #549/#557: pull every linked cargo-crate extension's
// `inventory::submit!` site into the daemon binary so its `register`
// fn pointer survives dead-code elimination. Bare `extern crate`
// suffices — no symbols need importing into source.
extern crate vulcan_core_ext_safety as _vulcan_core_ext_safety;
extern crate vulcan_core_ext_skills as _vulcan_core_ext_skills;
extern crate vulcan_ext_auto_commit as _vulcan_ext_auto_commit;
extern crate vulcan_ext_input_demo as _vulcan_ext_input_demo;
extern crate vulcan_ext_snake as _vulcan_ext_snake;
extern crate vulcan_ext_spinner_demo as _vulcan_ext_spinner_demo;
extern crate vulcan_ext_todo as _vulcan_ext_todo;

use clap::{CommandFactory, Parser};
use clap_complete::generate;
use std::fs::File;
use std::io::{Write as _, stdout};
// use std::io::Write; // covered by Write as _ above
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;
#[cfg(feature = "gateway")]
use vulcan::cli::GatewaySubcommand;
use vulcan::cli::{Cli, Command};
use vulcan::config::Config;
use vulcan::provider::StreamEvent;
use vulcan::tui::{ResumeTarget, run_tui};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // YYC-183: `vulcan doctor` must run even if Config::load
    // fails — diagnosing a broken config is the whole point.
    // Run it before the load step so a parse error surfaces as
    // a check instead of an unhandled abort.
    if matches!(cli.command, Some(Command::Doctor)) {
        let home = vulcan::config::vulcan_home();
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let report = vulcan::doctor::run_checks(&home, &cwd);
        print!("{}", vulcan::doctor::render_human(&report));
        if matches!(report.overall(), vulcan::doctor::CheckStatus::Fail) {
            std::process::exit(1);
        }
        return Ok(());
    }
    // `gateway init` is a config repair/bootstrap command, so it must be
    // able to run before the strongly-typed config loader rejects a missing
    // or partial `[gateway]` table. Use typed config only for the provider
    // label when it is readable; the command itself edits via toml_edit.
    #[cfg(feature = "gateway")]
    if let Some(Command::Gateway {
        cmd: Some(GatewaySubcommand::Init { force }),
        ..
    }) = cli.command
    {
        let dir = vulcan::config::vulcan_home();
        let config = match Config::load() {
            Ok(config) => config,
            Err(e) => {
                let config = Config::default();
                let _log_guard = init_cli_logging(&config);
                let _process_span = process_span("gateway").entered();
                tracing::warn!(
                    "config load failed before gateway init; continuing with defaults: {e}"
                );
                vulcan::cli_gateway::init(&dir, &config, force)?;
                return Ok(());
            }
        };
        let _log_guard = init_cli_logging(&config);
        let _process_span = process_span("gateway").entered();
        vulcan::cli_gateway::init(&dir, &config, force)?;
        return Ok(());
    }
    let config = Config::load()?;

    // YYC-264: when `--seed-cortex` is passed and cortex is enabled, seed
    // the knowledge graph from recent SQLite sessions before the session
    // starts. Non-fatal — failures are logged and we continue.
    if cli.seed_cortex
        && config.cortex.enabled
        && let Err(e) = vulcan::cli_cortex::seed_from_sessions(3).await
    {
        tracing::warn!("cortex seed failed: {e}");
    }

    macro_rules! init_cli_observability {
        ($command:literal) => {
            let _log_guard = init_cli_logging(&config);
            let _process_span = process_span($command).entered();
        };
    }

    macro_rules! init_tui_observability {
        ($command:literal) => {
            let _log_guard = init_tui_logging(&config);
            let _process_span = process_span($command).entered();
        };
    }

    match cli.command {
        None | Some(Command::Chat) => {
            init_tui_observability!("chat");
            let resume = if cli.resume {
                // --resume takes priority over --continue
                ResumeTarget::Pick
            } else if cli.r#continue {
                ResumeTarget::Last
            } else {
                ResumeTarget::None
            };
            run_tui(&config, resume, cli.profile.clone()).await?;
        }
        Some(Command::Prompt { text }) => {
            init_cli_observability!("prompt");
            #[cfg(feature = "daemon")]
            if !cli.no_daemon {
                match prompt_via_daemon(text.clone(), cli.profile.clone()).await {
                    Ok(()) => {}
                    Err(e) => {
                        tracing::warn!("daemon prompt failed, falling back to direct: {e}");
                        run_prompt_direct(&config, text, cli.profile.clone()).await?;
                    }
                }
            } else {
                run_prompt_direct(&config, text, cli.profile.clone()).await?;
            }
            #[cfg(not(feature = "daemon"))]
            run_prompt_direct(&config, text, cli.profile.clone()).await?;
        }
        Some(Command::Session { id }) => {
            init_tui_observability!("session");
            run_tui(&config, ResumeTarget::Specific(id), cli.profile.clone()).await?;
        }
        Some(Command::Search { query, limit }) => {
            init_cli_observability!("search");
            #[cfg(feature = "daemon")]
            if !cli.no_daemon {
                match search_via_daemon(query.clone(), limit).await {
                    Ok(()) => {}
                    Err(e) => {
                        tracing::warn!("daemon search failed, falling back to direct: {e}");
                        run_search_direct(&query, limit).await?;
                    }
                }
            } else {
                run_search_direct(&query, limit).await?;
            }
            #[cfg(not(feature = "daemon"))]
            run_search_direct(&query, limit).await?;
        }
        #[cfg(feature = "gateway")]
        Some(Command::Gateway { cmd, bind }) => {
            init_cli_observability!("gateway");
            match cmd.unwrap_or(GatewaySubcommand::Run { bind: None }) {
                GatewaySubcommand::Run { bind: run_bind } => {
                    vulcan::gateway::run(&config, run_bind.or(bind)).await?
                }
                GatewaySubcommand::Init { .. } => unreachable!("handled before Config::load"),
            }
        }
        Some(Command::MigrateConfig { force }) => {
            init_cli_observability!("migrate-config");
            let dir = vulcan::config::vulcan_home();
            let report = vulcan::config::Config::migrate(&dir, force)?;
            if !report.main_rewritten {
                println!("Nothing to migrate — config.toml already split (or absent).");
            } else {
                if report.keybinds_written {
                    println!("Wrote {}/keybinds.toml", dir.display());
                }
                if report.providers_written {
                    println!("Wrote {}/providers.toml", dir.display());
                }
                println!("Updated {}/config.toml (sections removed).", dir.display());
            }
        }
        Some(Command::Provider { cmd }) => {
            init_cli_observability!("provider");
            let dir = vulcan::config::vulcan_home();
            vulcan::cli_provider::run(cmd, dir).await?;
        }
        Some(Command::Model { cmd }) => {
            init_cli_observability!("model");
            vulcan::cli_model::run(cmd).await?;
        }
        Some(Command::Auth(args)) => {
            init_cli_observability!("auth");
            let dir = vulcan::config::vulcan_home();
            vulcan::cli_auth::run(args, dir).await?;
        }
        Some(Command::Completions { shell }) => {
            // YYC-213: dump a shell-completion script to stdout so
            // users can `vulcan completions bash > /path/to/file` (or
            // pipe into their shell init). Stays out of the logging
            // setup so the output is the script alone.
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "vulcan", &mut std::io::stdout());
        }
        Some(Command::Run { cmd }) => {
            init_cli_observability!("run");
            vulcan::cli_run::run(cmd).await?;
        }
        Some(Command::Artifact { cmd }) => {
            init_cli_observability!("artifact");
            vulcan::cli_artifact::run(cmd).await?;
        }
        Some(Command::Knowledge { cmd }) => {
            init_cli_observability!("knowledge");
            vulcan::cli_knowledge::run(cmd).await?;
        }
        Some(Command::ContextPack { cmd }) => {
            init_cli_observability!("context-pack");
            vulcan::cli_context_pack::run(cmd).await?;
        }
        Some(Command::Config { cmd }) => {
            init_cli_observability!("config");
            vulcan::cli_config::run(cmd).await?;
        }
        Some(Command::Trust { cmd }) => {
            init_cli_observability!("trust");
            vulcan::cli_trust::run(cmd).await?;
        }
        Some(Command::Review { cmd }) => {
            init_cli_observability!("review");
            vulcan::cli_review::run(cmd).await?;
        }
        Some(Command::Doctor) => unreachable!("handled before Config::load above"),
        Some(Command::Release { range }) => {
            init_cli_observability!("release");
            // YYC-221: ingest git log → build summary → render.
            let commits = vulcan::release::ingest_git_log(&range)?;
            let summary = vulcan::release::build_summary(&range, &commits);
            print!("{}", vulcan::release::render_markdown(&summary));
        }
        Some(Command::Policy { cmd }) => {
            init_cli_observability!("policy");
            vulcan::cli_policy::run(cmd).await?;
        }
        Some(Command::Replay { cmd }) => {
            init_cli_observability!("replay");
            vulcan::cli_replay::run(cmd).await?;
        }
        Some(Command::Impact { target, save }) => {
            init_cli_observability!("impact");
            vulcan::cli_impact::run(&target, save).await?;
        }
        Some(Command::Playbook { cmd }) => {
            init_cli_observability!("playbook");
            vulcan::cli_playbook::run(cmd).await?;
        }
        Some(Command::Extension { cmd }) => {
            init_cli_observability!("extension");
            vulcan::cli_extension::run(cmd).await?;
        }
        Some(Command::Cortex { cmd }) => {
            init_cli_observability!("cortex");
            #[cfg(feature = "daemon")]
            {
                if cli.no_daemon {
                    vulcan::cli_cortex::run(cmd).await?;
                } else {
                    vulcan::cli_cortex::run_with_client(cmd).await?;
                }
            }
            #[cfg(not(feature = "daemon"))]
            {
                vulcan::cli_cortex::run(cmd).await?;
            }
        }
        #[cfg(feature = "daemon")]
        Some(Command::Daemon { action }) => {
            init_cli_observability!("daemon");
            vulcan::daemon::cli::run(action).await?;
        }
        #[cfg(feature = "daemon")]
        Some(Command::HiddenPing) => {
            init_cli_observability!("hidden-ping");
            let client = vulcan::client::Client::connect_or_autostart().await?;
            let result = client.call("daemon.ping", serde_json::json!({})).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
    }

    Ok(())
}

fn process_span(command: &'static str) -> tracing::Span {
    tracing::info_span!(
        vulcan::observability::span::PROCESS,
        command,
        surface = "process"
    )
}

/// Log to stderr for CLI/one-shot mode — fine because there's no TUI
fn init_cli_logging(config: &Config) -> vulcan::observability::ObservabilityGuard {
    let filter = EnvFilter::try_from_env("VULCAN_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    vulcan::observability::init_with_writer(&config.observability, filter, std::io::stderr, true)
        .unwrap_or_else(|err| {
            eprintln!("Vulcan logging initialization failed: {err:#}");
            vulcan::observability::ObservabilityGuard::disabled()
        })
}

/// YYC-200: outcome of selecting the TUI log destination. The
/// `Sink` variant carries the original error so the user-facing
/// banner can explain *why* file logging is disabled instead of
/// silently dropping logs.
enum TuiLogTarget {
    File { file: File, path: PathBuf },
    Sink { reason: std::io::Error },
}

/// YYC-200: pick the writer for TUI logging without panicking. The
/// previous fallback opened `/dev/null`, which doesn't exist on
/// Windows and panicked via `unwrap()`. We now degrade to
/// `std::io::sink` and surface the failure reason.
fn pick_tui_log_target(log_path: PathBuf) -> TuiLogTarget {
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match File::create(&log_path) {
        Ok(file) => TuiLogTarget::File {
            file,
            path: log_path,
        },
        Err(reason) => TuiLogTarget::Sink { reason },
    }
}

/// Log to a file for TUI mode so the alternate screen stays clean
fn init_tui_logging(config: &Config) -> vulcan::observability::ObservabilityGuard {
    let log_path = vulcan::config::vulcan_home().join("vulcan.log");
    let filter = EnvFilter::try_from_env("VULCAN_LOG").unwrap_or_else(|_| EnvFilter::new("info"));

    match pick_tui_log_target(log_path) {
        TuiLogTarget::File { file, path } => {
            let guard =
                vulcan::observability::init_with_writer(&config.observability, filter, file, false)
                    .unwrap_or_else(|err| {
                        eprintln!("Vulcan logging initialization failed: {err:#}");
                        vulcan::observability::ObservabilityGuard::disabled()
                    });
            eprintln!("Vulcan TUI starting... logs → {path:?}");
            guard
        }
        TuiLogTarget::Sink { reason } => {
            let guard = vulcan::observability::init_with_writer(
                &config.observability,
                filter,
                std::io::sink,
                false,
            )
            .unwrap_or_else(|err| {
                eprintln!("Vulcan logging initialization failed: {err:#}");
                vulcan::observability::ObservabilityGuard::disabled()
            });
            eprintln!("Vulcan TUI starting... log file unavailable ({reason}); logs disabled.");
            guard
        }
    }
}

// ── Slice 2: direct-mode helpers (fallback when daemon is unavailable) ──

async fn run_prompt_direct(
    config: &Config,
    text: String,
    profile: Option<String>,
) -> anyhow::Result<()> {
    let mut agent = vulcan::agent::Agent::builder(config)
        .with_tool_profile(profile)
        .build()
        .await?;
    let (tx, mut rx) =
        tokio::sync::mpsc::channel::<StreamEvent>(vulcan::provider::STREAM_CHANNEL_CAPACITY);
    let stream_task = tokio::spawn(async move { agent.run_prompt_stream(&text, tx).await });

    let mut stdout = stdout();
    let mut exit_code = 0;
    while let Some(ev) = rx.recv().await {
        match ev {
            StreamEvent::Text(chunk) => {
                let _ = stdout.write_all(chunk.as_bytes());
                let _ = stdout.flush();
            }
            StreamEvent::Error(msg) => {
                eprintln!("\nError: {msg}");
                exit_code = 1;
            }
            StreamEvent::Done(_) => break,
            _ => {}
        }
    }
    let _ = writeln!(stdout);
    let _ = stdout.flush();
    stream_task.await??;
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

async fn run_search_direct(query: &str, limit: usize) -> anyhow::Result<()> {
    let store = vulcan::memory::SessionStore::try_new()?;
    let hits = store.search_messages(query, limit)?;
    if hits.is_empty() {
        println!("No matches.");
    } else {
        for h in hits {
            let preview: String = h.content.chars().take(120).collect();
            println!(
                "[{}…] {} (score {:.2})\n  {}\n",
                &h.session_id[..8],
                h.role,
                h.score,
                preview.replace('\n', " ")
            );
        }
    }
    Ok(())
}

// ── Slice 2: daemon-routed client helpers ──

#[cfg(feature = "daemon")]
async fn prompt_via_daemon(text: String, _profile: Option<String>) -> anyhow::Result<()> {
    let client = vulcan::client::Client::connect_or_autostart().await?;
    // For CLI: use prompt.run (buffered, simpler) — the streaming
    // TUI uses prompt.stream.
    let result = client
        .call("prompt.run", serde_json::json!({ "text": text }))
        .await?;
    println!("{}", result["text"].as_str().unwrap_or(""));
    Ok(())
}

#[cfg(feature = "daemon")]
async fn search_via_daemon(query: String, limit: usize) -> anyhow::Result<()> {
    let client = vulcan::client::Client::connect_or_autostart().await?;
    let result = client
        .call(
            "session.search",
            serde_json::json!({ "query": query, "limit": limit }),
        )
        .await?;
    let hits = result["results"].as_array().cloned().unwrap_or_default();
    if hits.is_empty() {
        println!("No matches.");
    } else {
        for h in hits {
            let sid = h["session_id"].as_str().unwrap_or("?");
            let preview = h["content"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(120)
                .collect::<String>();
            println!(
                "[{}…] {} (score {:.2})\n  {}\n",
                &sid[..sid.len().min(8)],
                h["role"].as_str().unwrap_or("?"),
                h["score"].as_f64().unwrap_or(0.0),
                preview.replace('\n', " ")
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// YYC-213: the bash completion script must mention global flags
    /// like `--profile` so we know the Cli struct's flag definitions
    /// are flowing through the generator. If a future PR drops a
    /// flag from the Cli, this test catches the silent regression
    /// in completion output.
    #[test]
    fn bash_completion_script_includes_global_profile_flag() {
        let mut cmd = vulcan::cli::Cli::command();
        let mut buf: Vec<u8> = Vec::new();
        generate(clap_complete::Shell::Bash, &mut cmd, "vulcan", &mut buf);
        let script = String::from_utf8(buf).expect("bash completion is utf-8");
        assert!(
            script.contains("--profile"),
            "bash completion script missing --profile flag"
        );
        assert!(
            script.contains("vulcan"),
            "bash completion script missing vulcan binary name"
        );
    }

    #[test]
    fn pick_tui_log_target_uses_sink_when_path_invalid() {
        // Use a path whose parent contains a NUL byte — File::create
        // rejects this on every platform, exercising the fallback
        // without depending on filesystem perms.
        let bad: PathBuf = ["\0bad", "vulcan.log"].iter().collect();
        match pick_tui_log_target(bad) {
            TuiLogTarget::Sink { .. } => {}
            TuiLogTarget::File { .. } => panic!("expected Sink fallback for invalid path"),
        }
    }

    #[test]
    fn pick_tui_log_target_returns_file_when_writable() {
        let tmp = std::env::temp_dir().join(format!("vulcan-tui-log-{}.log", std::process::id()));
        let outcome = pick_tui_log_target(tmp.clone());
        let _ = std::fs::remove_file(&tmp);
        match outcome {
            TuiLogTarget::File { path, .. } => assert_eq!(path, tmp),
            TuiLogTarget::Sink { reason } => panic!("expected File variant, got sink: {reason}"),
        }
    }
}
