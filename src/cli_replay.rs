//! YYC-184: `vulcan replay inspect` CLI driver.

use anyhow::{Result, anyhow};

use crate::cli::ReplaySubcommand;
use crate::replay::render_inspect;
use crate::run_record::{RunId, RunStore, SqliteRunStore};

pub async fn run(cmd: ReplaySubcommand) -> Result<()> {
    match cmd {
        ReplaySubcommand::Inspect { id } => inspect(&id),
    }
}

fn inspect(raw_id: &str) -> Result<()> {
    let store = SqliteRunStore::try_new()?;
    let id = resolve(&store, raw_id)?;
    let rec = store
        .get(id)?
        .ok_or_else(|| anyhow!("run record {id} not found"))?;
    print!("{}", render_inspect(&rec));
    Ok(())
}

fn resolve(store: &SqliteRunStore, raw: &str) -> Result<RunId> {
    if let Ok(uuid) = uuid::Uuid::parse_str(raw) {
        return Ok(RunId::from_uuid(uuid));
    }
    let recent = store.recent(500)?;
    let matches: Vec<_> = recent
        .iter()
        .filter(|r| r.id.to_string().starts_with(raw))
        .collect();
    match matches.len() {
        0 => Err(anyhow!("no run record matches `{raw}`")),
        1 => Ok(matches[0].id),
        n => Err(anyhow!(
            "id prefix `{raw}` matched {n} runs — supply more characters"
        )),
    }
}
