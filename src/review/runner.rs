//! YYC-190 PR-2: bounded critic pass driver.
//!
//! Builds a fresh agent under the `reviewer` capability profile
//! (read-only + cargo_check; no `bash` / `write_file` /
//! `git_commit`), sends the critic system prompt + the target
//! text, and parses the response back into a [`ReviewReport`].
//!
//! Persists the rendered markdown as a YYC-180 `Report` artifact
//! when an artifact store is supplied.

use anyhow::{Context, Result};

use super::{
    CRITIC_SYSTEM_PROMPT, ReviewKind, ReviewReport, build_critic_user_message, parse_markdown,
    render_markdown,
};
use crate::artifact::{Artifact, ArtifactKind, ArtifactStore};
use crate::config::Config;

/// Result of a bounded review pass: the parsed report plus the
/// canonical markdown the model emitted (so callers can persist
/// the raw response if they care).
pub struct ReviewOutcome {
    pub report: ReviewReport,
    pub markdown: String,
}

/// Run a bounded critic pass on `target` under the `reviewer`
/// capability profile. Builds a fresh agent so review mode never
/// inherits write capabilities from the parent; the agent loop is
/// capped at 4 iterations so the critic can't drift into a long
/// tool-using turn.
pub async fn run_review(config: &Config, kind: ReviewKind, target: &str) -> Result<ReviewOutcome> {
    let prompt = build_critic_user_message(&kind, target);
    let mut agent = crate::agent::Agent::builder(config)
        .with_max_iterations(4)
        .with_tool_profile(Some("reviewer".to_string()))
        .build()
        .await
        .context("build reviewer agent")?;
    let system_with_critic = format!("{CRITIC_SYSTEM_PROMPT}\n");
    // Stamp the critic system prompt into the saved history so
    // the agent loop's `BeforePrompt` injections happen on top of
    // it. Easiest path: inject as a User message that the model
    // treats as instructions plus the target.
    let response = agent
        .run_prompt(&format!("{system_with_critic}\n{prompt}"))
        .await
        .context("reviewer agent run failed")?;
    let report = parse_markdown(&response)?;
    let markdown = render_markdown(&kind, &report);
    Ok(ReviewOutcome { report, markdown })
}

/// Persist the review report as a YYC-180 `Report` artifact and
/// return its id. Callers pass `None` for `store` when artifact
/// persistence isn't wired (one-shot CLI w/o an agent handle).
pub fn persist_report(
    store: Option<&dyn ArtifactStore>,
    kind: &ReviewKind,
    outcome: &ReviewOutcome,
    session_id: Option<String>,
) -> Result<Option<crate::artifact::ArtifactId>> {
    let store = match store {
        Some(s) => s,
        None => return Ok(None),
    };
    let title = match kind {
        ReviewKind::Plan => "Plan review",
        ReviewKind::Diff => "Diff review",
        ReviewKind::Run => "Run review",
        ReviewKind::Issue => "Issue review",
        ReviewKind::Other(_) => "Review",
    };
    let mut art = Artifact::inline_text(ArtifactKind::Report, outcome.markdown.clone())
        .with_source("review")
        .with_title(title);
    if let Some(s) = session_id {
        art = art.with_session_id(s);
    }
    let id = art.id;
    store.create(&art)?;
    Ok(Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::InMemoryArtifactStore;

    #[test]
    fn persist_report_writes_report_artifact_with_review_source() {
        let store = InMemoryArtifactStore::new();
        let outcome = ReviewOutcome {
            report: ReviewReport::new(),
            markdown: "# Diff review\n\n## Findings\n\n_No findings._\n".to_string(),
        };
        let id = persist_report(
            Some(&store),
            &ReviewKind::Diff,
            &outcome,
            Some("sess-1".into()),
        )
        .unwrap()
        .expect("artifact persisted");
        let got = store.get(id).unwrap().unwrap();
        assert_eq!(got.kind, ArtifactKind::Report);
        assert_eq!(got.source.as_deref(), Some("review"));
        assert_eq!(got.title.as_deref(), Some("Diff review"));
        assert_eq!(got.session_id.as_deref(), Some("sess-1"));
        assert!(got.content.unwrap().contains("_No findings._"));
    }

    #[test]
    fn persist_report_is_a_noop_without_store() {
        let outcome = ReviewOutcome {
            report: ReviewReport::new(),
            markdown: String::new(),
        };
        let id = persist_report(None, &ReviewKind::Run, &outcome, None).unwrap();
        assert!(id.is_none());
    }
}
