pub mod agent;
pub mod cli;
pub mod cli_auth;
pub mod cli_provider;
pub mod code;
pub mod config;
pub mod context;
#[cfg(feature = "gateway")]
pub mod gateway;
pub mod hooks;
pub mod memory;
pub mod orchestration;
pub mod pause;
pub mod platform;
pub mod prompt_builder;
pub mod provider;
pub mod run_record;
pub mod skills;
pub mod tools;
pub mod tui;

// Re-export for convenience
pub use provider::*;
