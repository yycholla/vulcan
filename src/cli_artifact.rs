//! YYC-180: `vulcan artifact list/show` — query the durable
//! artifact store. Read-only; mutation lands through the agent
//! itself.

use anyhow::{Context, Result, anyhow};
use owo_colors::OwoColorize;
use std::io::IsTerminal;
use uuid::Uuid;

use crate::artifact::{Artifact, ArtifactId, ArtifactKind, ArtifactStore, SqliteArtifactStore};
use crate::cli::ArtifactSubcommand;
use crate::run_record::{RunId, RunStore, SqliteRunStore};

pub async fn run(cmd: Option<ArtifactSubcommand>) -> Result<()> {
    let store = SqliteArtifactStore::try_new().context("open ~/.vulcan/artifacts.db")?;
    match cmd {
        None => interactive_select(&store),
        Some(ArtifactSubcommand::List {
            limit,
            run,
            session,
        }) => list(&store, limit, run.as_deref(), session.as_deref()),
        Some(ArtifactSubcommand::Show { id }) => show(&store, &id),
    }
}

fn interactive_select(store: &SqliteArtifactStore) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "vulcan artifact (interactive) requires a terminal. Use `vulcan artifact list` to browse, or `vulcan artifact show <id>` to inspect an artifact."
        );
    }

    let items = store.recent(20)?;
    if items.is_empty() {
        println!("No artifacts.");
        return Ok(());
    }

    let labels: Vec<String> = items.iter().map(artifact_picker_label).collect();
    println!();
    let pick = dialoguer::FuzzySelect::with_theme(&dialoguer::theme::ColorfulTheme::default())
        .with_prompt("Pick an artifact")
        .items(&labels)
        .default(0)
        .interact()
        .context("picker cancelled")?;

    print!("{}", render_artifact_show(&items[pick]));
    Ok(())
}

fn list<S: ArtifactStore + ?Sized>(
    store: &S,
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
    print!("{}", render_artifact_list(&items));
    Ok(())
}

fn show<S: ArtifactStore + ?Sized>(store: &S, raw_id: &str) -> Result<()> {
    let id = resolve_artifact_id(store, raw_id)?;
    let art = store
        .get(id)?
        .ok_or_else(|| anyhow!("artifact {id} not found"))?;
    print!("{}", render_artifact_show(&art));
    Ok(())
}

fn render_artifact_list(items: &[Artifact]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{:<10} {:<20} {:<14} {:<10} {:<22} {:<22} {}\n",
        "id".bold(),
        "type".bold(),
        "session".bold(),
        "run".bold(),
        "created".bold(),
        "source".bold(),
        "title".bold(),
    ));
    for art in items {
        out.push_str(&format!(
            "{:<10} {:<20} {:<14} {:<10} {:<22} {:<22} {}\n",
            short_id(&art.id.to_string()),
            kind_badge(art.kind),
            truncate(art.session_id.as_deref().unwrap_or("-"), 14),
            art.run_id
                .map(|r| short_id(&r.to_string()))
                .unwrap_or_else(|| "-".into()),
            art.created_at.format("%Y-%m-%d %H:%M:%S"),
            truncate(art.source.as_deref().unwrap_or("-"), 22),
            art.title.as_deref().unwrap_or("-"),
        ));
    }
    out
}

fn render_artifact_show(art: &Artifact) -> String {
    let mut out = String::new();
    out.push_str(&format!("{} {}\n", "Artifact".bold(), art.id));
    out.push_str(&format!("{}\n", "Metadata".bold().underline()));
    out.push_str(&format!("  type:       {}\n", kind_badge(art.kind)));
    out.push_str(&format!(
        "  session:    {}\n",
        art.session_id.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "  run:        {}\n",
        art.run_id
            .map(|r| r.to_string())
            .unwrap_or_else(|| "-".into())
    ));
    out.push_str(&format!(
        "  parent:     {}\n",
        art.parent_artifact_id
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".into())
    ));
    out.push_str(&format!(
        "  source:     {}\n",
        art.source.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "  title:      {}\n",
        art.title.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "  redaction:  {}\n",
        art.redaction.0.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "  created:    {}\n",
        art.created_at.format("%Y-%m-%d %H:%M:%S%.3f UTC")
    ));
    if let Some(path) = &art.external_path {
        out.push_str(&format!("  external:   {path}\n"));
    }
    if let Some(content) = &art.content {
        out.push_str(&format!(
            "\n{} ({} bytes)\n",
            "Content".bold().underline(),
            content.len()
        ));
        out.push_str(content);
        if !content.ends_with('\n') {
            out.push('\n');
        }
    }
    out
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

fn kind_badge(kind: ArtifactKind) -> String {
    let label = format!("[{}]", kind.as_str());
    match kind {
        ArtifactKind::Plan => label.blue().bold().to_string(),
        ArtifactKind::Diff => label.yellow().bold().to_string(),
        ArtifactKind::Report => label.green().bold().to_string(),
        ArtifactKind::ToolOutput => label.cyan().bold().to_string(),
        ArtifactKind::SubagentSummary => label.magenta().bold().to_string(),
        ArtifactKind::LogExcerpt => label.dimmed().to_string(),
    }
}

fn artifact_picker_label(art: &Artifact) -> String {
    format!(
        "{}  {:<18}  {:<18}  {}",
        short_id(&art.id.to_string()),
        format!("[{}]", art.kind.as_str()),
        truncate(art.source.as_deref().unwrap_or("-"), 18),
        art.title.as_deref().unwrap_or("-")
    )
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn truncate(s: &str, max_chars: usize) -> String {
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars && max_chars > 1 {
        out.pop();
        out.push('…');
    }
    out
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

    #[test]
    fn render_artifact_list_includes_badge_source_and_title() {
        let art = Artifact::inline_text(ArtifactKind::Report, "findings")
            .with_session_id("session-1")
            .with_source("review")
            .with_title("Review report");

        let out = render_artifact_list(&[art]);

        assert!(out.contains("id"));
        assert!(out.contains("type"));
        assert!(out.contains("session"));
        assert!(out.contains("source"));
        assert!(out.contains("[report]"));
        assert!(out.contains("session-1"));
        assert!(out.contains("review"));
        assert!(out.contains("Review report"));
    }

    #[test]
    fn render_artifact_show_includes_metadata_and_content_section() {
        let art = Artifact::inline_text(ArtifactKind::Plan, "phase 1\nphase 2")
            .with_source("planner")
            .with_title("Plan");

        let out = render_artifact_show(&art);

        assert!(out.contains("Artifact"));
        assert!(out.contains("Metadata"));
        assert!(out.contains("Content"));
        assert!(out.contains("[plan]"));
        assert!(out.contains("planner"));
        assert!(out.contains("phase 1"));
    }
}
