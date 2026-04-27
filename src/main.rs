use clap::Parser;
use std::io::Write;
use tracing_subscriber::EnvFilter;
use vulcan::cli::{Cli, Command};
use vulcan::config::Config;
use vulcan::provider::StreamEvent;
use vulcan::tui::{ResumeTarget, run_tui};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = Config::load()?;

    match cli.command {
        None | Some(Command::Chat) => {
            init_tui_logging();
            let resume = if cli.resume {
                // --resume takes priority over --continue
                ResumeTarget::Pick
            } else if cli.r#continue {
                ResumeTarget::Last
            } else {
                ResumeTarget::None
            };
            run_tui(&config, resume).await?;
        }
        Some(Command::Prompt { text }) => {
            init_cli_logging();
            let mut agent = vulcan::agent::Agent::builder(&config).build().await?;
            if cli.r#continue {
                agent.continue_last_session()?;
            }
            // YYC-38: stream tokens to stdout as they arrive instead of
            // blocking on the buffered chat path. Long generations now
            // start producing visible output immediately.
            let (tx, mut rx) = tokio::sync::mpsc::channel::<StreamEvent>(
                vulcan::provider::STREAM_CHANNEL_CAPACITY,
            );
            let stream_task = tokio::spawn(async move { agent.run_prompt_stream(&text, tx).await });

            let mut stdout = std::io::stdout().lock();
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
                    // Reasoning, ToolCallStart/End not surfaced in CLI
                    // output — they'd mix with the response stream and
                    // need a richer renderer (the TUI handles them).
                    _ => {}
                }
            }
            // Trailing newline so the next shell prompt isn't glued to
            // the model's last token.
            let _ = writeln!(stdout);
            let _ = stdout.flush();
            // Surface any task-level error (provider init, etc).
            stream_task.await??;
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
        }
        Some(Command::Session { id }) => {
            init_tui_logging();
            run_tui(&config, ResumeTarget::Specific(id)).await?;
        }
        Some(Command::Search { query, limit }) => {
            init_cli_logging();
            let store = vulcan::memory::SessionStore::new();
            let hits = store.search_messages(&query, limit)?;
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
        }
        #[cfg(feature = "gateway")]
        Some(Command::Gateway { bind }) => {
            init_cli_logging();
            vulcan::gateway::run(&config, bind).await?;
        }
        Some(Command::MigrateConfig { force }) => {
            init_cli_logging();
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
            init_cli_logging();
            let dir = vulcan::config::vulcan_home();
            vulcan::cli_provider::run(cmd, dir).await?;
        }
        Some(Command::Auth(args)) => {
            init_cli_logging();
            let dir = vulcan::config::vulcan_home();
            vulcan::cli_auth::run(args, dir).await?;
        }
    }

    Ok(())
}

/// Log to stderr for CLI/one-shot mode — fine because there's no TUI
fn init_cli_logging() {
    let filter = EnvFilter::try_from_env("VULCAN_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// Log to a file for TUI mode so the alternate screen stays clean
fn init_tui_logging() {
    let log_dir = vulcan::config::vulcan_home();
    std::fs::create_dir_all(&log_dir).ok();
    let log_path = log_dir.join("vulcan.log");

    let file = std::fs::File::create(&log_path)
        .unwrap_or_else(|_| std::fs::File::open("/dev/null").unwrap());

    let filter = EnvFilter::try_from_env("VULCAN_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file)
        .with_ansi(false)
        .init();

    eprintln!("Vulcan TUI starting... logs → {log_path:?}");
}
