use clap::Parser;
use tracing_subscriber::EnvFilter;
use vulcan::cli::{Cli, Command};
use vulcan::config::Config;
use vulcan::tui::{ResumeTarget, run_tui};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = Config::load()?;

    match cli.command {
        None | Some(Command::Chat) => {
            init_tui_logging();
            let resume = if cli.r#continue {
                ResumeTarget::Last
            } else {
                ResumeTarget::None
            };
            run_tui(&config, resume).await?;
        }
        Some(Command::Prompt { text }) => {
            init_cli_logging();
            let mut agent = vulcan::agent::Agent::new(&config)?;
            if cli.r#continue {
                agent.continue_last_session()?;
            }
            let response = agent.run_prompt(&text).await?;
            println!("{response}");
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
    }

    Ok(())
}

/// Log to stderr for CLI/one-shot mode — fine because there's no TUI
fn init_cli_logging() {
    let filter = EnvFilter::try_from_env("VULCAN_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info"));
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

    let file = std::fs::File::create(&log_path).unwrap_or_else(|_| {
        std::fs::File::open("/dev/null").unwrap()
    });

    let filter = EnvFilter::try_from_env("VULCAN_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file)
        .with_ansi(false)
        .init();

    eprintln!("Vulcan TUI starting... logs → {log_path:?}");
}
