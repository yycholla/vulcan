use crate::config::Config;
use anyhow::Result;

pub mod agent_map;
pub mod lane;
pub mod loopback;
pub mod queue;
pub mod registry;

pub async fn run(_config: &Config, _bind_override: Option<String>) -> Result<()> {
    anyhow::bail!("gateway not yet implemented")
}
