//! YYC-190: bounded critic-pass primitives.
//!
//! ## Scope of this PR
//!
//! - `Severity`, `Finding`, `ReviewKind`, `ReviewReport` types.
//! - Markdown rendering of a report (`render_markdown`).
//! - Parser for the canonical findings-first format the critic
//!   prompt (PR-2) will ask the model to emit.
//!
//! ## Deliberately deferred
//!
//! - Built-in critic prompt template + agent driver (PR-2).
//! - `vulcan review plan|diff|run` CLI surface (PR-3).
//! - Persisting the report as a YYC-180 `Report` artifact at the
//!   end of a review run (PR-2).

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Coarse severity ladder. Higher tiers are blocking; lower tiers
/// are advisory. Kept simple — finer grades can land per-team
/// later if the prompt warrants it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }

    /// Parse from any case (`Info` / `INFO` / `info`). Returns
    /// `None` on unknown input so callers can fall back instead
    /// of panicking on an unexpected severity tag from the model.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "info" => Some(Severity::Info),
            "low" => Some(Severity::Low),
            "medium" | "med" => Some(Severity::Medium),
            "high" => Some(Severity::High),
            "critical" | "crit" => Some(Severity::Critical),
            _ => None,
        }
    }
}

/// What this review pass is critiquing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewKind {
    Plan,
    Diff,
    Run,
    Issue,
    Other(String),
}

/// One reviewer finding. Hard fields (severity + summary) are
/// required; the rest are best-effort context the critic provides
/// when it can.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    pub summary: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub evidence: Option<String>,
    pub suggestion: Option<String>,
}

/// Structured output of a review pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ReviewReport {
    pub findings: Vec<Finding>,
    pub questions: Vec<String>,
    pub residual_risks: Vec<String>,
}

impl ReviewReport {
    pub fn new() -> Self {
        Self::default()
    }

    /// True when the report contains a `High` or `Critical`
    /// finding. Callers (review-mode CLI, future hooks) use this
    /// to gate auto-apply paths.
    pub fn has_blocking_finding(&self) -> bool {
        self.findings
            .iter()
            .any(|f| matches!(f.severity, Severity::High | Severity::Critical))
    }
}

/// Render the report as the canonical findings-first markdown
/// the project expects from a review pass.
pub fn render_markdown(kind: &ReviewKind, report: &ReviewReport) -> String {
    let mut out = String::new();
    let kind_str = match kind {
        ReviewKind::Plan => "Plan",
        ReviewKind::Diff => "Diff",
        ReviewKind::Run => "Run",
        ReviewKind::Issue => "Issue",
        ReviewKind::Other(s) => s.as_str(),
    };
    out.push_str(&format!("# {kind_str} review\n\n"));

    out.push_str("## Findings\n\n");
    if report.findings.is_empty() {
        out.push_str("_No findings._\n\n");
    } else {
        for f in &report.findings {
            out.push_str(&format!(
                "- **[{sev}]** {sum}",
                sev = f.severity.as_str(),
                sum = f.summary
            ));
            if let (Some(file), Some(line)) = (&f.file, f.line) {
                out.push_str(&format!(" (`{file}:{line}`)"));
            } else if let Some(file) = &f.file {
                out.push_str(&format!(" (`{file}`)"));
            }
            out.push('\n');
            if let Some(ev) = &f.evidence {
                out.push_str(&format!("  - evidence: {ev}\n"));
            }
            if let Some(s) = &f.suggestion {
                out.push_str(&format!("  - suggestion: {s}\n"));
            }
        }
        out.push('\n');
    }

    if !report.questions.is_empty() {
        out.push_str("## Open questions\n\n");
        for q in &report.questions {
            out.push_str(&format!("- {q}\n"));
        }
        out.push('\n');
    }

    if !report.residual_risks.is_empty() {
        out.push_str("## Residual risks\n\n");
        for r in &report.residual_risks {
            out.push_str(&format!("- {r}\n"));
        }
        out.push('\n');
    }

    out
}

/// Parse a critic response into a [`ReviewReport`]. Recognises
/// the headings emitted by [`render_markdown`] so a round-trip
/// is lossless on the canonical format. Models that drift slightly
/// (different bullet markers, extra whitespace) still parse —
/// the parser is intentionally tolerant of cosmetic noise.
pub fn parse_markdown(raw: &str) -> Result<ReviewReport> {
    let mut report = ReviewReport::new();
    let mut section = Section::Other;
    let mut pending_finding: Option<Finding> = None;

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(stripped) = trimmed.strip_prefix("## ") {
            // Flush any in-progress finding before switching
            // sections — `evidence:`/`suggestion:` continuation
            // lines belong to the most recent bullet.
            if let Some(f) = pending_finding.take() {
                report.findings.push(f);
            }
            let head = stripped.to_ascii_lowercase();
            section = if head.contains("finding") {
                Section::Findings
            } else if head.contains("question") {
                Section::Questions
            } else if head.contains("residual") || head.contains("risk") {
                Section::Risks
            } else {
                Section::Other
            };
            continue;
        }

        match section {
            Section::Findings => {
                if let Some(f) = parse_finding_line(trimmed) {
                    if let Some(prev) = pending_finding.take() {
                        report.findings.push(prev);
                    }
                    pending_finding = Some(f);
                } else if let Some(f) = pending_finding.as_mut() {
                    apply_continuation(f, trimmed);
                }
            }
            Section::Questions => {
                if let Some(q) = bullet_text(trimmed) {
                    report.questions.push(q);
                }
            }
            Section::Risks => {
                if let Some(r) = bullet_text(trimmed) {
                    report.residual_risks.push(r);
                }
            }
            Section::Other => {}
        }
    }

    if let Some(f) = pending_finding.take() {
        report.findings.push(f);
    }
    Ok(report)
}

#[derive(Copy, Clone)]
enum Section {
    Findings,
    Questions,
    Risks,
    Other,
}

fn bullet_text(line: &str) -> Option<String> {
    line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .map(|s| s.trim().to_string())
}

fn parse_finding_line(line: &str) -> Option<Finding> {
    let body = bullet_text(line)?;
    // Expect `**[severity]** summary [(`file:line`)]`.
    let (sev, rest) = body
        .strip_prefix("**[")
        .and_then(|after| after.split_once("]**").map(|(s, r)| (s, r.trim_start())))?;
    let severity = Severity::parse(sev)?;

    // Pull off optional ` (\`file:line\`)` trailer.
    let mut summary = rest.trim().to_string();
    let mut file: Option<String> = None;
    let mut line_no: Option<u32> = None;
    if let Some(start) = summary.rfind(" (`") {
        if let Some(end) = summary.rfind("`)") {
            if end > start {
                let loc = &summary[start + 3..end];
                let (f, l) = match loc.split_once(':') {
                    Some((f, l)) => (f.to_string(), l.parse::<u32>().ok()),
                    None => (loc.to_string(), None),
                };
                file = Some(f);
                line_no = l;
                summary.truncate(start);
                summary = summary.trim_end().to_string();
            }
        }
    }

    Some(Finding {
        severity,
        summary,
        file,
        line: line_no,
        evidence: None,
        suggestion: None,
    })
}

fn apply_continuation(f: &mut Finding, line: &str) {
    let body = match bullet_text(line) {
        Some(b) => b,
        None => return,
    };
    if let Some(rest) = body.strip_prefix("evidence:") {
        f.evidence = Some(rest.trim().to_string());
    } else if let Some(rest) = body.strip_prefix("suggestion:") {
        f.suggestion = Some(rest.trim().to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_finding() -> Finding {
        Finding {
            severity: Severity::High,
            summary: "missing test for empty-input branch".to_string(),
            file: Some("src/foo.rs".to_string()),
            line: Some(42),
            evidence: Some("`fn parse(\"\")` returns Ok(()) without assertion".to_string()),
            suggestion: Some("add `parse(\"\").unwrap_err()` regression test".to_string()),
        }
    }

    #[test]
    fn severity_round_trips_through_lowercase_strings() {
        for s in [
            Severity::Info,
            Severity::Low,
            Severity::Medium,
            Severity::High,
            Severity::Critical,
        ] {
            let parsed = Severity::parse(s.as_str()).unwrap();
            assert_eq!(parsed, s);
        }
        // Tolerant aliases.
        assert_eq!(Severity::parse("MED").unwrap(), Severity::Medium);
        assert_eq!(Severity::parse("CRIT").unwrap(), Severity::Critical);
        assert!(Severity::parse("nonsense").is_none());
    }

    #[test]
    fn has_blocking_finding_classifies_correctly() {
        let mut report = ReviewReport::new();
        assert!(!report.has_blocking_finding());
        report.findings.push(Finding {
            severity: Severity::Low,
            summary: "nit".into(),
            file: None,
            line: None,
            evidence: None,
            suggestion: None,
        });
        assert!(!report.has_blocking_finding());
        report.findings.push(sample_finding());
        assert!(report.has_blocking_finding());
    }

    #[test]
    fn render_markdown_round_trips_through_parser() {
        let mut report = ReviewReport::new();
        report.findings.push(sample_finding());
        report.findings.push(Finding {
            severity: Severity::Low,
            summary: "consider naming".into(),
            file: None,
            line: None,
            evidence: None,
            suggestion: None,
        });
        report
            .questions
            .push("is this path covered by a test?".into());
        report
            .residual_risks
            .push("legacy callers may still rely on the old shape".into());

        let md = render_markdown(&ReviewKind::Diff, &report);
        let parsed = parse_markdown(&md).unwrap();
        assert_eq!(parsed, report);
    }

    #[test]
    fn parser_tolerates_extra_whitespace_and_alt_bullets() {
        // Mix of canonical bullet (`-`) and asterisk bullet (`*`),
        // extra leading whitespace, and continuation lines under
        // a finding. File:line carries the canonical
        // ` (\`file:line\`)` trailer.
        let raw = r#"# Plan review

## Findings

  *   **[high]** flaky test (`src/foo.rs:7`)
    - evidence: ignored when timed out
    - suggestion:  remove `#[ignore]`
* **[INFO]** style nit

## Questions

- did the migration include a backfill?
"#;
        let report = parse_markdown(raw).unwrap();
        assert_eq!(report.findings.len(), 2);
        let f0 = &report.findings[0];
        assert_eq!(f0.severity, Severity::High);
        assert_eq!(f0.file.as_deref(), Some("src/foo.rs"));
        assert_eq!(f0.line, Some(7));
        assert!(f0.evidence.as_deref().unwrap().contains("ignored"));
        assert!(f0.suggestion.as_deref().unwrap().contains("ignore"));
        assert_eq!(report.findings[1].severity, Severity::Info);
        assert_eq!(report.questions.len(), 1);
    }

    #[test]
    fn empty_findings_section_renders_no_findings_marker() {
        let report = ReviewReport::new();
        let md = render_markdown(&ReviewKind::Plan, &report);
        assert!(md.contains("_No findings._"));
        let parsed = parse_markdown(&md).unwrap();
        assert!(parsed.findings.is_empty());
    }
}
