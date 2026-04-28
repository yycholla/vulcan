//! YYC-180: `vulcan artifact list/show` — query the durable
//! artifact store. Read-only; mutation lands through the agent
//! itself.

use anyhow::{Context, Result, anyhow};
use uuid::Uuid;

use crate::artifact::{Artifact, ArtifactId, ArtifactStore, SqliteArtifactStore};
use crate::cli::ArtifactSubcommand;
use crate::run_record::{RunId, RunStore, SqliteRunStore};

pub async fn run(cmd: ArtifactSubcommand) -> Result<()> {
    let store = SqliteArtifactStore::try_new().context("open ~/.vulcan/artifacts.db")?;
    match cmd {
        ArtifactSubcommand::List {
            limit,
            run,
            session,
        } => list(&store, limit, run.as_deref(), session.as_deref()),
        ArtifactSubcommand::Show { id } => show(&store, &id),
    }
}

fn list(
    store: &SqliteArtifactStore,
    limit: usize,
    run_filter: Option<&str>,
    session_filter: Option<&str>,
) -> Result<()> {
    let items = if let Some(raw) = run_filter {
        let run_id = resolve_run_id(raw)?;
        store.list_for_run(run_id)?
    } else if let Some(s) = session_filter {
        store.list_for_session(s)?
    } else {
        store.recent(limit)?
    };
    if items.is_empty() {
        println!("No artifacts.");
        return Ok(());
    }
    println!(
        "{:<10} {:<18} {:<22} {:<28} title",
        "id", "kind", "created", "source"
    );
    for art in items {
        let id_short: String = art.id.to_string().chars().take(8).collect();
        let created = art.created_at.format("%Y-%m-%d %H:%M:%S");
        let source = art.source.unwrap_or_else(|| "-".into());
        let title = art.title.unwrap_or_else(|| "-".into());
        let kind = art.kind.as_str();
        println!("{id_short:<10} {kind:<18} {created:<22} {source:<28} {title}");
    }
    Ok(())
}

fn show(store: &SqliteArtifactStore, raw_id: &str) -> Result<()> {
    let id = resolve_artifact_id(store, raw_id)?;
    let art = store
        .get(id)?
        .ok_or_else(|| anyhow!("artifact {id} not found"))?;
    println!("Artifact {id}");
    println!("  kind:       {}", art.kind.as_str());
    println!("  session:    {}", art.session_id.as_deref().unwrap_or("-"));
    println!(
        "  run:        {}",
        art.run_id
            .map(|r| r.to_string())
            .unwrap_or_else(|| "-".into())
    );
    println!(
        "  parent:     {}",
        art.parent_artifact_id
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".into())
    );
    println!("  source:     {}", art.source.as_deref().unwrap_or("-"));
    println!("  title:      {}", art.title.as_deref().unwrap_or("-"));
    println!(
        "  redaction:  {}",
        art.redaction.0.as_deref().unwrap_or("-")
    );
    println!(
        "  created:    {}",
        art.created_at.format("%Y-%m-%d %H:%M:%S%.3f UTC")
    );
    if let Some(path) = &art.external_path {
        println!("  external:   {path}");
    }
    if let Some(content) = &art.content {
        println!("\n--- content ({} bytes) ---", content.len());
        println!("{content}");
    }
    Ok(())
}

fn resolve_artifact_id<S: ArtifactStore + ?Sized>(store: &S, raw_id: &str) -> Result<ArtifactId> {
    if let Ok(uuid) = Uuid::parse_str(raw_id) {
        return Ok(ArtifactId::from_uuid(uuid));
    }
    let recent = store.recent(500)?;
    let matches: Vec<&Artifact> = recent
        .iter()
        .filter(|a| a.id.to_string().starts_with(raw_id))
        .collect();
    match matches.len() {
        0 => Err(anyhow!("no artifact matches `{raw_id}`")),
        1 => Ok(matches[0].id),
        n => Err(anyhow!(
            "id prefix `{raw_id}` matched {n} artifacts — supply more characters"
        )),
    }
}

fn resolve_run_id(raw_id: &str) -> Result<RunId> {
    if let Ok(uuid) = Uuid::parse_str(raw_id) {
        return Ok(RunId::from_uuid(uuid));
    }
    let runs = SqliteRunStore::try_new()?.recent(500)?;
    let matches: Vec<_> = runs
        .iter()
        .filter(|r| r.id.to_string().starts_with(raw_id))
        .collect();
    match matches.len() {
        0 => Err(anyhow!("no run record matches `{raw_id}`")),
        1 => Ok(matches[0].id),
        n => Err(anyhow!(
            "id prefix `{raw_id}` matched {n} runs — supply more characters"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::{Artifact, ArtifactKind, InMemoryArtifactStore};

    #[test]
    fn resolve_artifact_id_accepts_full_uuid() {
        let store = InMemoryArtifactStore::new();
        let art = Artifact::inline_text(ArtifactKind::Plan, "p");
        let id = art.id;
        store.create(&art).unwrap();
        let resolved = resolve_artifact_id(&store, &id.to_string()).unwrap();
        assert_eq!(resolved, id);
    }

    #[test]
    fn resolve_artifact_id_accepts_prefix() {
        let store = InMemoryArtifactStore::new();
        let art = Artifact::inline_text(ArtifactKind::Plan, "p");
        let id = art.id;
        store.create(&art).unwrap();
        let prefix: String = id.to_string().chars().take(8).collect();
        let resolved = resolve_artifact_id(&store, &prefix).unwrap();
        assert_eq!(resolved, id);
    }

    #[test]
    fn resolve_artifact_id_errors_on_no_match() {
        let store = InMemoryArtifactStore::new();
        let err = resolve_artifact_id(&store, "abcd1234").unwrap_err();
        assert!(err.to_string().contains("no artifact matches"));
    }
}
