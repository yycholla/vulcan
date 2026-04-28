//! YYC-189: change impact planner — structured impact report
//! types + canonical markdown rendering.
//!
//! ## Scope of this PR
//!
//! - `RiskLevel`, `ImpactSource`, `AffectedKind` enums.
//! - `ImpactItem` + `ImpactReport` data shapes with evidence
//!   provenance fields.
//! - `render_markdown` + `parse_markdown` round-trip on the
//!   canonical format.
//!
//! ## Deliberately deferred
//!
//! - Heuristic generator (code-graph + references + search) —
//!   lands when a CLI driver / agent flow is wired.
//! - `vulcan impact <file|symbol>` CLI surface.
//! - Auto-suggest from edit hooks for high-risk edits.

use anyhow::Result;
use serde::{Deserialize, Serialize};

mod generator;
pub use generator::generate_for_file;

/// How confident the planner is about an entry. `Evidence` means
/// the item was sourced from a deterministic index hit (call
/// graph / references / search). `Guess` means the planner
/// inferred the item from heuristics (filename match, related
/// path, etc.) — readers should treat it as advisory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Evidence,
    Guess,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Evidence => "evidence",
            Confidence::Guess => "guess",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "evidence" => Some(Confidence::Evidence),
            "guess" => Some(Confidence::Guess),
            _ => None,
        }
    }
}

/// Where the planner sourced an entry. Lets review-mode + replay
/// trace a finding back to the index that surfaced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactSource {
    CodeGraph,
    References,
    EmbeddingSearch,
    RipgrepSearch,
    Docs,
    Heuristic,
}

impl ImpactSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ImpactSource::CodeGraph => "code_graph",
            ImpactSource::References => "references",
            ImpactSource::EmbeddingSearch => "embedding_search",
            ImpactSource::RipgrepSearch => "ripgrep_search",
            ImpactSource::Docs => "docs",
            ImpactSource::Heuristic => "heuristic",
        }
    }
}

/// Coarse risk classification for the change as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

impl RiskLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            RiskLevel::Low => "low",
            RiskLevel::Medium => "medium",
            RiskLevel::High => "high",
            RiskLevel::Critical => "critical",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "low" => Some(RiskLevel::Low),
            "medium" | "med" => Some(RiskLevel::Medium),
            "high" => Some(RiskLevel::High),
            "critical" | "crit" => Some(RiskLevel::Critical),
            _ => None,
        }
    }
}

/// One predicted-affected item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpactItem {
    pub path: String,
    pub symbol: Option<String>,
    pub source: ImpactSource,
    pub confidence: Confidence,
    pub note: Option<String>,
}

/// Suggested verification step. Free-form because the planner may
/// emit shell commands, test names, or `cargo test` invocations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationStep {
    pub command: String,
    pub rationale: Option<String>,
}

/// Structured impact report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ImpactReport {
    pub target: String,
    pub affected_modules: Vec<ImpactItem>,
    pub affected_tests: Vec<ImpactItem>,
    pub affected_docs: Vec<ImpactItem>,
    pub recommended_verifications: Vec<VerificationStep>,
    pub risk: Option<RiskLevel>,
    pub rationale: Option<String>,
    pub rollback: Option<String>,
}

impl ImpactReport {
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            ..Self::default()
        }
    }
}

/// Canonical markdown rendering.
pub fn render_markdown(report: &ImpactReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Impact: {}\n\n", report.target));

    if let Some(level) = report.risk {
        out.push_str(&format!("**Risk:** {}\n\n", level.as_str()));
    }

    let sections: &[(&str, &Vec<ImpactItem>)] = &[
        ("Affected modules", &report.affected_modules),
        ("Affected tests", &report.affected_tests),
        ("Affected docs", &report.affected_docs),
    ];
    for (heading, items) in sections {
        out.push_str(&format!("## {heading}\n\n"));
        if items.is_empty() {
            out.push_str("_None._\n\n");
            continue;
        }
        for item in items.iter() {
            out.push_str(&format!(
                "- `{path}`{sym} — source={src} confidence={conf}",
                path = item.path,
                sym = item
                    .symbol
                    .as_deref()
                    .map(|s| format!(" ({s})"))
                    .unwrap_or_default(),
                src = item.source.as_str(),
                conf = item.confidence.as_str(),
            ));
            if let Some(note) = &item.note {
                out.push_str(&format!(" — {note}"));
            }
            out.push('\n');
        }
        out.push('\n');
    }

    out.push_str("## Recommended verifications\n\n");
    if report.recommended_verifications.is_empty() {
        out.push_str("_None._\n\n");
    } else {
        for v in &report.recommended_verifications {
            out.push_str(&format!("- `{}`", v.command));
            if let Some(r) = &v.rationale {
                out.push_str(&format!(" — {r}"));
            }
            out.push('\n');
        }
        out.push('\n');
    }

    if let Some(r) = &report.rationale {
        out.push_str(&format!("## Rationale\n\n{r}\n\n"));
    }
    if let Some(rb) = &report.rollback {
        out.push_str(&format!("## Rollback\n\n{rb}\n"));
    }

    out
}

/// Reverse of [`render_markdown`]. Tolerant enough that small
/// model drift (trailing whitespace, alt bullets) still parses.
pub fn parse_markdown(raw: &str) -> Result<ImpactReport> {
    let mut report = ImpactReport::default();
    let mut section = Section::Other;
    let mut multiline_buf: Vec<String> = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# Impact: ") {
            report.target = rest.trim().to_string();
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("**Risk:** ") {
            report.risk = RiskLevel::parse(rest.trim_end_matches('*').trim());
            continue;
        }
        if let Some(stripped) = trimmed.strip_prefix("## ") {
            // Flush any rationale/rollback buffer before
            // switching sections.
            flush_multiline(&mut report, section, &mut multiline_buf);
            section = match stripped.to_ascii_lowercase().as_str() {
                s if s.contains("affected modules") => Section::Modules,
                s if s.contains("affected tests") => Section::Tests,
                s if s.contains("affected docs") => Section::Docs,
                s if s.contains("recommended verification") => Section::Verifications,
                s if s.contains("rationale") => Section::Rationale,
                s if s.contains("rollback") => Section::Rollback,
                _ => Section::Other,
            };
            continue;
        }
        if trimmed.is_empty() && matches!(section, Section::Rationale | Section::Rollback) {
            multiline_buf.push(String::new());
            continue;
        }
        match section {
            Section::Modules => {
                if let Some(item) = parse_item_bullet(trimmed) {
                    report.affected_modules.push(item);
                }
            }
            Section::Tests => {
                if let Some(item) = parse_item_bullet(trimmed) {
                    report.affected_tests.push(item);
                }
            }
            Section::Docs => {
                if let Some(item) = parse_item_bullet(trimmed) {
                    report.affected_docs.push(item);
                }
            }
            Section::Verifications => {
                if let Some(v) = parse_verification_bullet(trimmed) {
                    report.recommended_verifications.push(v);
                }
            }
            Section::Rationale | Section::Rollback => {
                multiline_buf.push(trimmed.to_string());
            }
            Section::Other => {}
        }
    }
    flush_multiline(&mut report, section, &mut multiline_buf);
    Ok(report)
}

#[derive(Copy, Clone)]
enum Section {
    Modules,
    Tests,
    Docs,
    Verifications,
    Rationale,
    Rollback,
    Other,
}

fn flush_multiline(report: &mut ImpactReport, section: Section, buf: &mut Vec<String>) {
    if buf.is_empty() {
        return;
    }
    let text = buf.join("\n").trim().to_string();
    match section {
        Section::Rationale => {
            if !text.is_empty() {
                report.rationale = Some(text);
            }
        }
        Section::Rollback => {
            if !text.is_empty() {
                report.rollback = Some(text);
            }
        }
        _ => {}
    }
    buf.clear();
}

fn parse_item_bullet(line: &str) -> Option<ImpactItem> {
    let body = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))?;
    let body = body.trim();
    // Format: `path`[ (symbol)] — source=X confidence=Y[ — note]
    let (path_with_sym, after) = body.split_once(" — ")?;
    let path_with_sym = path_with_sym.trim();
    let path_str = path_with_sym
        .strip_prefix('`')
        .and_then(|s| s.split_once('`'))
        .map(|(p, _)| p)?;
    let symbol = path_with_sym.find('(').and_then(|i| {
        let close = path_with_sym.rfind(')')?;
        if close > i + 1 {
            Some(path_with_sym[i + 1..close].to_string())
        } else {
            None
        }
    });
    let mut source = ImpactSource::Heuristic;
    let mut confidence = Confidence::Guess;
    let mut note: Option<String> = None;
    let mut tail = after;
    if let Some((before, after_note)) = after.split_once(" — ") {
        tail = before;
        note = Some(after_note.trim().to_string());
    }
    for part in tail.split_whitespace() {
        if let Some(rest) = part.strip_prefix("source=") {
            source = match rest {
                "code_graph" => ImpactSource::CodeGraph,
                "references" => ImpactSource::References,
                "embedding_search" => ImpactSource::EmbeddingSearch,
                "ripgrep_search" => ImpactSource::RipgrepSearch,
                "docs" => ImpactSource::Docs,
                _ => ImpactSource::Heuristic,
            };
        } else if let Some(rest) = part.strip_prefix("confidence=") {
            if let Some(c) = Confidence::parse(rest) {
                confidence = c;
            }
        }
    }
    Some(ImpactItem {
        path: path_str.to_string(),
        symbol,
        source,
        confidence,
        note,
    })
}

fn parse_verification_bullet(line: &str) -> Option<VerificationStep> {
    let body = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))?;
    let body = body.trim();
    let (cmd_part, rationale_part) = match body.split_once(" — ") {
        Some((c, r)) => (c.trim(), Some(r.trim().to_string())),
        None => (body, None),
    };
    let cmd = cmd_part
        .strip_prefix('`')
        .and_then(|s| s.split_once('`'))
        .map(|(c, _)| c.to_string())?;
    Some(VerificationStep {
        command: cmd,
        rationale: rationale_part,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> ImpactReport {
        let mut r = ImpactReport::new("rename `Foo::bar` to `Foo::baz`");
        r.affected_modules.push(ImpactItem {
            path: "src/foo.rs".into(),
            symbol: Some("Foo::bar".into()),
            source: ImpactSource::CodeGraph,
            confidence: Confidence::Evidence,
            note: Some("definition site".into()),
        });
        r.affected_modules.push(ImpactItem {
            path: "src/lib.rs".into(),
            symbol: None,
            source: ImpactSource::References,
            confidence: Confidence::Evidence,
            note: None,
        });
        r.affected_tests.push(ImpactItem {
            path: "tests/foo.rs".into(),
            symbol: None,
            source: ImpactSource::RipgrepSearch,
            confidence: Confidence::Guess,
            note: Some("test mentions `Foo::bar`".into()),
        });
        r.recommended_verifications.push(VerificationStep {
            command: "cargo test --features gateway".into(),
            rationale: Some("changes touch the gateway feature".into()),
        });
        r.risk = Some(RiskLevel::Medium);
        r.rationale = Some("symbol used by 2 callers; tests likely cover both".into());
        r.rollback = Some("revert via `git revert`; no schema changes".into());
        r
    }

    #[test]
    fn render_round_trips_through_parser() {
        let r = sample_report();
        let md = render_markdown(&r);
        let back = parse_markdown(&md).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn empty_report_renders_no_findings_markers() {
        let r = ImpactReport::new("noop");
        let md = render_markdown(&r);
        assert!(md.contains("# Impact: noop"));
        assert!(md.contains("_None._"));
    }

    #[test]
    fn parse_recovers_target_and_risk() {
        let raw = "# Impact: refactor X\n\n**Risk:** high\n\n## Affected modules\n\n_None._\n";
        let r = parse_markdown(raw).unwrap();
        assert_eq!(r.target, "refactor X");
        assert_eq!(r.risk, Some(RiskLevel::High));
    }

    #[test]
    fn risk_ordering_is_meaningful() {
        assert!(RiskLevel::Low < RiskLevel::Medium);
        assert!(RiskLevel::Medium < RiskLevel::High);
        assert!(RiskLevel::High < RiskLevel::Critical);
    }
}
