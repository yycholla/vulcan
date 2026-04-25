use clap::Parser;
use vulcan::cli::{Cli, Command};
use vulcan::config::Config;
use vulcan::tui::run_tui;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = Config::load()?;

    match cli.command {
        None | Some(Command::Chat) => {
            // TUI mode: log to a file so tracing output doesn't splat into the TUI
            init_tui_logging();
            run_tui(&config).await?;
        }
        Some(Command::Prompt { text }) => {
            // One-shot mode: log to stderr (visible while waiting)
            init_cli_logging();
            let mut agent = vulcan::agent::Agent::new(&config);
            let response = agent.run_prompt(&text).await?;
            println!("{response}");
        }
        Some(Command::Session { id }) => {
            init_cli_logging();
            let mut agent = vulcan::agent::Agent::new(&config);
            agent.resume_session(&id).await?;
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
        // Fallback: /dev/null
        std::fs::File::open("/dev/null").unwrap()
    });

    let filter = EnvFilter::try_from_env("VULCAN_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file)
        .with_ansi(false)
        .init();

    // Also print a note to stderr before the TUI takes over
    eprintln!("Vulcan TUI starting... logs → {log_path:?}");
}
