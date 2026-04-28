//! YYC-191: release + changelog assistant.
//!
//! ## Scope of this PR
//!
//! - `ReleaseSummary` shape: completed items grouped by area,
//!   commit list with attribution, risks, verification list.
//! - `CommitInfo` extracted from `git log` output via the `git2`
//!   bindings already in the dep tree (gix). Local-first; no
//!   network calls.
//! - Markdown rendering of the summary.
//! - Tests covering grouping, risk highlighting, and empty input.
//!
//! ## Deliberately deferred
//!
//! - Linear issue lookups (waits on a CLI driver).
//! - Run-record / artifact aggregation.
//! - Release-notes draft generation via the model (separate PR).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitInfo {
    pub sha: String,
    pub author: String,
    pub date: String,
    pub subject: String,
    /// Issue ids extracted from the subject (any token matching
    /// `YYC-<digits>`). Sorted, unique. Used to group commits by
    /// area.
    pub linked_issues: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ReleaseSummary {
    pub range: String,
    pub commits: Vec<CommitInfo>,
    /// Issue id → list of commit shas that mention it. Drives the
    /// "completed issues grouped by area" view.
    pub commits_by_issue: Vec<(String, Vec<String>)>,
    /// Free-form risk callouts (e.g. "modifies SQLite schema").
    pub risks: Vec<String>,
    /// Verification commands the summary recommends running
    /// before tagging.
    pub verifications: Vec<String>,
}

impl ReleaseSummary {
    pub fn new(range: impl Into<String>) -> Self {
        Self {
            range: range.into(),
            ..Self::default()
        }
    }
}

/// Pull issue ids out of a commit subject. Conservative —
/// matches `YYC-<digits>` only, not other tracker prefixes.
pub fn extract_issue_ids(subject: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let bytes = subject.as_bytes();
    let mut i = 0usize;
    while i + 4 <= bytes.len() {
        if &bytes[i..i + 4] == b"YYC-" {
            let mut j = i + 4;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 4 {
                let id = &subject[i..j];
                let id_owned = id.to_string();
                if !found.contains(&id_owned) {
                    found.push(id_owned);
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    found.sort();
    found
}

/// Build a [`ReleaseSummary`] from a list of `(sha, author,
/// date, subject)` tuples — the shape `git log --pretty=...`
/// emits. Keeps the parser pure-Rust + testable; the runner
/// (PR-2) feeds real git output into this same function.
pub fn build_summary(range: &str, commits: &[(String, String, String, String)]) -> ReleaseSummary {
    let mut summary = ReleaseSummary::new(range);
    let mut by_issue: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for (sha, author, date, subject) in commits {
        let ids = extract_issue_ids(subject);
        for id in &ids {
            by_issue.entry(id.clone()).or_default().push(sha.clone());
        }
        summary.commits.push(CommitInfo {
            sha: sha.clone(),
            author: author.clone(),
            date: date.clone(),
            subject: subject.clone(),
            linked_issues: ids,
        });
    }
    summary.commits_by_issue = by_issue.into_iter().collect();

    // Conservative risk surfacing — these subject keywords are
    // strong signals of a change that needs extra verification
    // before release. Matches case-insensitively.
    for c in &summary.commits {
        let lc = c.subject.to_ascii_lowercase();
        if lc.contains("schema")
            || lc.contains("migration")
            || lc.contains("breaking")
            || lc.contains("revert")
        {
            summary
                .risks
                .push(format!("`{}` — {}", short_sha(&c.sha), c.subject));
        }
    }

    summary
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect()
}

pub fn render_markdown(summary: &ReleaseSummary) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Release summary: {}\n\n", summary.range));

    out.push_str("## Issues completed\n\n");
    if summary.commits_by_issue.is_empty() {
        out.push_str("_No tracked issues in this range._\n\n");
    } else {
        for (issue, shas) in &summary.commits_by_issue {
            out.push_str(&format!("- {issue} ({} commit(s))", shas.len()));
            if !shas.is_empty() {
                out.push_str(": ");
                let labels: Vec<String> = shas.iter().map(|s| short_sha(s)).collect();
                out.push_str(&labels.join(", "));
            }
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str("## Commits\n\n");
    if summary.commits.is_empty() {
        out.push_str("_No commits._\n\n");
    } else {
        for c in &summary.commits {
            out.push_str(&format!(
                "- `{}` {} — {} ({})\n",
                short_sha(&c.sha),
                c.subject,
                c.author,
                c.date
            ));
        }
        out.push('\n');
    }

    out.push_str("## Risks\n\n");
    if summary.risks.is_empty() {
        out.push_str("_None flagged._\n\n");
    } else {
        for r in &summary.risks {
            out.push_str(&format!("- {r}\n"));
        }
        out.push('\n');
    }

    if !summary.verifications.is_empty() {
        out.push_str("## Verifications\n\n");
        for v in &summary.verifications {
            out.push_str(&format!("- {v}\n"));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<(String, String, String, String)> {
        vec![
            (
                "abc123def456".into(),
                "Alice".into(),
                "2026-04-28".into(),
                "YYC-179 PR-1: run record module".into(),
            ),
            (
                "def4567890ab".into(),
                "Alice".into(),
                "2026-04-28".into(),
                "YYC-179 PR-2: agent loop emits events".into(),
            ),
            (
                "11112222aaaa".into(),
                "Bob".into(),
                "2026-04-28".into(),
                "fix: typo in CLI help".into(),
            ),
            (
                "ffff00001111".into(),
                "Bob".into(),
                "2026-04-28".into(),
                "YYC-180: artifact schema migration".into(),
            ),
        ]
    }

    #[test]
    fn extract_issue_ids_finds_yyc_tokens() {
        assert_eq!(
            extract_issue_ids("YYC-179 PR-2: agent loop"),
            vec!["YYC-179".to_string()]
        );
        assert_eq!(
            extract_issue_ids("YYC-12 + YYC-345"),
            vec!["YYC-12".to_string(), "YYC-345".to_string()]
        );
        assert_eq!(
            extract_issue_ids("nothing tracked here"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn build_summary_groups_commits_by_issue() {
        let commits = fixture();
        let s = build_summary("main..HEAD", &commits);
        let yyc_179 = s
            .commits_by_issue
            .iter()
            .find(|(id, _)| id == "YYC-179")
            .unwrap();
        assert_eq!(yyc_179.1.len(), 2);
        let yyc_180 = s
            .commits_by_issue
            .iter()
            .find(|(id, _)| id == "YYC-180")
            .unwrap();
        assert_eq!(yyc_180.1.len(), 1);
    }

    #[test]
    fn build_summary_flags_migration_keywords_as_risk() {
        let commits = fixture();
        let s = build_summary("main..HEAD", &commits);
        assert!(s.risks.iter().any(|r| r.contains("migration")));
        // The non-risky commit should not be in the risk list.
        assert!(!s.risks.iter().any(|r| r.contains("typo")));
    }

    #[test]
    fn empty_input_renders_no_issues_marker() {
        let s = build_summary("v1..v2", &[]);
        let md = render_markdown(&s);
        assert!(md.contains("_No tracked issues in this range._"));
        assert!(md.contains("_No commits._"));
        assert!(md.contains("_None flagged._"));
    }

    #[test]
    fn render_markdown_includes_short_shas_and_subjects() {
        let commits = fixture();
        let s = build_summary("main..HEAD", &commits);
        let md = render_markdown(&s);
        assert!(md.contains("`abc123de`"));
        assert!(md.contains("typo in CLI help"));
        assert!(md.contains("YYC-179 (2 commit(s))"));
    }
}
