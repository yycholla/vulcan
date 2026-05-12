//! YYC-184: `vulcan replay inspect` CLI driver.

use anyhow::{Result, anyhow};

use crate::artifact::{ArtifactStore, SqliteArtifactStore};
use crate::cli::ReplaySubcommand;
use crate::replay::{render_inspect, render_report, simulate};
use crate::run_record::{RunId, RunStore, SqliteRunStore};

pub async fn run(cmd: ReplaySubcommand) -> Result<()> {
    match cmd {
        ReplaySubcommand::Inspect { id } => inspect(&id),
        ReplaySubcommand::Simulate { id } => simulate_run(&id),
    }
}

fn load_run(store: &SqliteRunStore, raw_id: &str) -> Result<crate::run_record::RunRecord> {
    let id = resolve(store, raw_id)?;
    store
        .get(id)?
        .ok_or_else(|| anyhow!("run record {id} not found"))
}

fn inspect(raw_id: &str) -> Result<()> {
    let store = SqliteRunStore::try_new()?;
    let rec = load_run(&store, raw_id)?;
    print!("{}", render_inspect(&rec));
    Ok(())
}

fn simulate_run(raw_id: &str) -> Result<()> {
    let run_store = SqliteRunStore::try_new()?;
    let rec = load_run(&run_store, raw_id)?;
    let artifact_store = SqliteArtifactStore::try_new()?;
    let artifacts = artifact_store.list_for_run(rec.id)?;
    let report = simulate(&rec, &artifacts);
    print!("{}", render_report(&report));
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
