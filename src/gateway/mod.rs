use crate::config::Config;
use anyhow::Result;

pub async fn run(_config: &Config, _bind_override: Option<String>) -> Result<()> {
    anyhow::bail!("gateway not yet implemented")
}
