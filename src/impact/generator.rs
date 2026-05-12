//! YYC-218: live impact generator (closes YYC-189 acceptance).
//!
//! Walks code graph + ripgrep + heuristics to populate an
//! [`ImpactReport`] for a target file. Designed to fail soft —
//! when an index isn't available, the corresponding section
//! falls back to `Heuristic` confidence rather than erroring.

use crate::code::ParserCache;
use crate::code::graph::{CodeGraph, CodeGraphEdge};
use anyhow::{Context, Result};
use ignore::WalkBuilder;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use super::{Confidence, ImpactItem, ImpactReport, ImpactSource, RiskLevel, VerificationStep};

/// Cap on how many ripgrep-style hits land in the report. Keeps
/// the markdown digestible when a symbol is referenced
/// everywhere; the user can re-run with a narrower target.
const MAX_REFERENCES_PER_SYMBOL: usize = 20;

/// Build an [`ImpactReport`] for a single file. The target path
/// is interpreted relative to `workspace_root` for display; the
/// scan walks `workspace_root` for references.
pub fn generate_for_file(workspace_root: &Path, target: &Path) -> Result<ImpactReport> {
    let canonical_target = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    let target_display = canonical_target
        .strip_prefix(workspace_root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| canonical_target.display().to_string());

    let mut report = ImpactReport::new(format!("file: {target_display}"));

    let symbols = extract_symbols(&canonical_target).unwrap_or_default();
    let symbols_were_found = !symbols.is_empty();

    // Affected modules: files (other than the target) that
    // textually reference any symbol defined in the target.
    let mut module_paths: BTreeSet<String> = BTreeSet::new();
    for symbol in &symbols {
        let hits = ripgrep_workspace(workspace_root, symbol, MAX_REFERENCES_PER_SYMBOL)?;
        for hit in hits {
            if hit == target_display {
                continue;
            }
            module_paths.insert(hit);
        }
    }
    for path in &module_paths {
        let confidence = if symbols_were_found {
            Confidence::Evidence
        } else {
            Confidence::Guess
        };
        report.affected_modules.push(ImpactItem {
            path: path.clone(),
            symbol: None,
            source: ImpactSource::RipgrepSearch,
            confidence,
            note: Some("references one or more symbols defined in the target".into()),
        });
    }

    // Affected tests: heuristic — anything under `tests/` whose
    // path mentions the target's stem, plus any `tests::` module
    // entries in `module_paths`.
    if let Some(stem) = canonical_target.file_stem().and_then(|s| s.to_str()) {
        let test_hits = list_workspace_tests_matching(workspace_root, stem)?;
        for hit in test_hits {
            if hit == target_display {
                continue;
            }
            report.affected_tests.push(ImpactItem {
                path: hit,
                symbol: None,
                source: ImpactSource::Heuristic,
                confidence: Confidence::Guess,
                note: Some(format!("filename mentions `{stem}`")),
            });
        }
    }

    // Affected docs: README / docs / wiki entries that mention
    // any symbol from the target.
    if symbols_were_found {
        for symbol in &symbols {
            let doc_hits = ripgrep_docs(workspace_root, symbol)?;
            for hit in doc_hits {
                report.affected_docs.push(ImpactItem {
                    path: hit,
                    symbol: Some(symbol.clone()),
                    source: ImpactSource::Docs,
                    confidence: Confidence::Guess,
                    note: Some("doc mentions symbol".into()),
                });
            }
        }
    }

    // Verifications: if there's a Cargo.toml at workspace root,
    // suggest the standard cargo invocations.
    if workspace_root.join("Cargo.toml").exists() {
        report.recommended_verifications.push(VerificationStep {
            command: "cargo build --all-targets".into(),
            rationale: Some("compile + warnings sanity check".into()),
        });
        report.recommended_verifications.push(VerificationStep {
            command: "cargo test".into(),
            rationale: Some("run unit + integration tests".into()),
        });
        report.recommended_verifications.push(VerificationStep {
            command: "cargo clippy --all-targets".into(),
            rationale: Some("lint pass".into()),
        });
    }

    // Risk inference: scope-based heuristic.
    report.risk = Some(infer_risk(
        report.affected_modules.len(),
        report.affected_tests.len(),
        symbols_were_found,
    ));
    report.rationale = Some(format!(
        "extracted {} symbol(s) from target; {} module + {} test references; \
         risk derived from reference fan-out",
        symbols.len(),
        report.affected_modules.len(),
        report.affected_tests.len(),
    ));

    Ok(report)
}

/// Build an [`ImpactReport`] for a named symbol. Code graph caller/callee
/// edges are evidence-backed findings; textual code/test/doc hits are kept as
/// reference/search evidence so partial indexes still produce useful output.
pub fn generate_for_symbol(workspace_root: &Path, symbol: &str) -> Result<ImpactReport> {
    let symbol = symbol.trim();
    let mut report = ImpactReport::new(format!("symbol: {symbol}"));
    let mut graph_evidence = 0usize;
    let mut graph_unavailable = false;

    match open_graph(workspace_root) {
        Ok(graph) => {
            graph_evidence += add_symbol_definitions(&mut report, &graph, symbol)?;
            let edge_evidence = add_code_graph_symbol_evidence(&mut report, &graph, symbol)?;
            graph_evidence += edge_evidence;
            if edge_evidence == 0 {
                graph_unavailable = true;
            }
        }
        Err(_) => graph_unavailable = true,
    }

    add_reference_search_evidence(&mut report, workspace_root, symbol)?;
    add_doc_evidence(&mut report, workspace_root, symbol)?;
    add_standard_verifications(&mut report, workspace_root);

    report.risk = Some(infer_risk(
        report.affected_modules.len(),
        report.affected_tests.len(),
        !symbol.is_empty(),
    ));
    report.rationale = Some(format!(
        "symbol `{symbol}` impact derived from {graph_evidence} code graph edge/definition hit(s), {} affected module(s), {} test reference(s), and {} doc hit(s){}",
        report.affected_modules.len(),
        report.affected_tests.len(),
        report.affected_docs.len(),
        if graph_unavailable {
            "; code graph index appears partial or unavailable, so reference/search findings may be incomplete"
        } else {
            ""
        }
    ));

    Ok(report)
}

/// Build an advisory report from free-form task text by extracting plausible
/// identifiers and aggregating their symbol reports.
pub fn generate_for_task(workspace_root: &Path, task: &str) -> Result<ImpactReport> {
    let mut report = ImpactReport::new(format!("task: {}", task.trim()));
    let symbols = extract_task_symbols(task);
    let mut notes = Vec::new();

    for symbol in &symbols {
        let symbol_report = generate_for_symbol(workspace_root, symbol)?;
        merge_items(&mut report.affected_modules, symbol_report.affected_modules);
        merge_items(&mut report.affected_tests, symbol_report.affected_tests);
        merge_items(&mut report.affected_docs, symbol_report.affected_docs);
        notes.push(format!("`{symbol}`"));
    }
    add_standard_verifications(&mut report, workspace_root);
    report.risk = Some(infer_risk(
        report.affected_modules.len(),
        report.affected_tests.len(),
        !symbols.is_empty(),
    ));
    report.rationale = Some(if symbols.is_empty() {
        "no plausible symbols found in task text; report contains only generic verification guidance".into()
    } else {
        format!(
            "task text mapped to candidate symbol(s): {}; findings aggregate code graph, reference search, and docs evidence where available",
            notes.join(", ")
        )
    });
    Ok(report)
}

fn open_graph(workspace_root: &Path) -> Result<CodeGraph> {
    CodeGraph::open(workspace_root.to_path_buf(), Arc::new(ParserCache::new()))
}

fn add_symbol_definitions(
    report: &mut ImpactReport,
    graph: &CodeGraph,
    symbol: &str,
) -> Result<usize> {
    let rows = graph.find_by_name(symbol, 20)?;
    let count = rows.len();
    for row in rows {
        push_unique_item(
            &mut report.affected_modules,
            ImpactItem {
                path: row.file,
                symbol: Some(row.name),
                source: ImpactSource::CodeGraph,
                confidence: Confidence::Evidence,
                note: Some(format!("{} definition from code graph index", row.kind)),
            },
        );
    }
    Ok(count)
}

fn add_code_graph_symbol_evidence(
    report: &mut ImpactReport,
    graph: &CodeGraph,
    symbol: &str,
) -> Result<usize> {
    let mut count = 0usize;
    let callers = graph.find_callers(symbol, 50)?;
    for edge in callers.edges {
        count += add_edge_item(report, edge, true, "calls target symbol");
    }
    let callees = graph.find_callees(symbol, 50)?;
    for edge in callees.edges {
        count += add_edge_item(report, edge, false, "called by target symbol");
    }
    Ok(count)
}

fn add_edge_item(
    report: &mut ImpactReport,
    edge: CodeGraphEdge,
    use_source: bool,
    note: &str,
) -> usize {
    let (path, symbol) = if use_source {
        (edge.source_file, edge.source_name)
    } else {
        (edge.target_file, edge.target_name)
    };
    push_unique_item(
        &mut report.affected_modules,
        ImpactItem {
            path,
            symbol,
            source: ImpactSource::CodeGraph,
            confidence: Confidence::Evidence,
            note: Some(note.into()),
        },
    );
    1
}

fn add_reference_search_evidence(
    report: &mut ImpactReport,
    workspace_root: &Path,
    symbol: &str,
) -> Result<()> {
    for hit in ripgrep_workspace(workspace_root, symbol, MAX_REFERENCES_PER_SYMBOL)? {
        if is_test_path(&hit) {
            push_unique_item(
                &mut report.affected_tests,
                ImpactItem {
                    path: hit,
                    symbol: Some(symbol.to_string()),
                    source: ImpactSource::RipgrepSearch,
                    confidence: Confidence::Evidence,
                    note: Some("test references symbol".into()),
                },
            );
        } else {
            push_unique_item(
                &mut report.affected_modules,
                ImpactItem {
                    path: hit,
                    symbol: Some(symbol.to_string()),
                    source: ImpactSource::References,
                    confidence: Confidence::Evidence,
                    note: Some("textual reference search hit".into()),
                },
            );
        }
    }
    Ok(())
}

fn add_doc_evidence(report: &mut ImpactReport, workspace_root: &Path, symbol: &str) -> Result<()> {
    for hit in ripgrep_docs(workspace_root, symbol)? {
        push_unique_item(
            &mut report.affected_docs,
            ImpactItem {
                path: hit,
                symbol: Some(symbol.to_string()),
                source: ImpactSource::Docs,
                confidence: Confidence::Evidence,
                note: Some("doc mentions symbol".into()),
            },
        );
    }
    Ok(())
}

fn add_standard_verifications(report: &mut ImpactReport, workspace_root: &Path) {
    if workspace_root.join("Cargo.toml").exists() {
        report.recommended_verifications.push(VerificationStep {
            command: "cargo build --all-targets".into(),
            rationale: Some("compile + warnings sanity check".into()),
        });
        report.recommended_verifications.push(VerificationStep {
            command: "cargo test".into(),
            rationale: Some("run unit + integration tests".into()),
        });
        report.recommended_verifications.push(VerificationStep {
            command: "cargo clippy --all-targets".into(),
            rationale: Some("lint pass".into()),
        });
    }
}

fn push_unique_item(items: &mut Vec<ImpactItem>, item: ImpactItem) {
    if !items.iter().any(|existing| {
        existing.path == item.path
            && existing.symbol == item.symbol
            && existing.source == item.source
    }) {
        items.push(item);
    }
}

fn merge_items(into: &mut Vec<ImpactItem>, from: Vec<ImpactItem>) {
    for item in from {
        push_unique_item(into, item);
    }
}

fn is_test_path(path: &str) -> bool {
    path.contains("/tests/")
        || path
            .rsplit_once('/')
            .map(|(_, name)| name)
            .unwrap_or(path)
            .contains("test")
}

fn extract_task_symbols(task: &str) -> Vec<String> {
    let mut symbols = BTreeSet::new();
    for token in task.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':')) {
        let token = token.trim_matches(':');
        if token.len() >= 3
            && token.chars().any(|c| c == '_' || c.is_ascii_uppercase())
            && token.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            symbols.insert(token.to_string());
        }
    }
    symbols.into_iter().take(20).collect()
}

fn infer_risk(modules: usize, tests: usize, has_symbols: bool) -> RiskLevel {
    if !has_symbols {
        // Couldn't parse the file — be pessimistic.
        return RiskLevel::Medium;
    }
    let fanout = modules + tests;
    if fanout >= 20 {
        RiskLevel::High
    } else if fanout >= 5 {
        RiskLevel::Medium
    } else {
        RiskLevel::Low
    }
}

fn extract_symbols(path: &Path) -> Result<Vec<String>> {
    use tree_sitter::{Query, QueryCursor, StreamingIterator};

    let lang = match crate::code::Language::from_path(path) {
        Some(l) => l,
        None => return Ok(Vec::new()),
    };
    let source =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;

    let parsers = std::sync::Arc::new(crate::code::ParserCache::new());
    let query_text = lang.outline_query();
    if query_text.is_empty() {
        return Ok(Vec::new());
    }

    let inner: Result<Vec<String>> = parsers.with_parser(lang, |parser| {
        let tree = parser
            .parse(&source, None)
            .ok_or_else(|| anyhow::anyhow!("parse failed"))?;
        let grammar: tree_sitter::Language = match lang {
            crate::code::Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            crate::code::Language::Python => tree_sitter_python::LANGUAGE.into(),
            crate::code::Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            crate::code::Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            crate::code::Language::Go => tree_sitter_go::LANGUAGE.into(),
            crate::code::Language::Json => tree_sitter_json::LANGUAGE.into(),
        };
        let query = Query::new(&grammar, query_text)
            .with_context(|| format!("compile outline query for {lang:?}"))?;
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());

        let mut out: BTreeSet<String> = BTreeSet::new();
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let name = query.capture_names()[cap.index as usize];
                // Outline queries name the symbol identifier capture
                // `@name`; other captures (`@function`, `@struct`,
                // etc.) point at the wrapping node and aren't useful
                // for cross-file textual reference search.
                if name != "name" {
                    continue;
                }
                if let Ok(text) = cap.node.utf8_text(source.as_bytes()) {
                    let trimmed = text.trim();
                    // Filter to plausible identifiers — drop
                    // anything containing whitespace, brackets,
                    // or punctuation that wouldn't compile as a
                    // bare symbol reference.
                    if !trimmed.is_empty()
                        && trimmed.chars().all(|c| c.is_alphanumeric() || c == '_')
                        && trimmed.len() >= 3
                    {
                        out.insert(trimmed.to_string());
                    }
                }
            }
        }
        Ok(out.into_iter().collect())
    })?;
    inner
}

fn ripgrep_workspace(root: &Path, symbol: &str, max_hits: usize) -> Result<Vec<String>> {
    let mut hits: BTreeSet<String> = BTreeSet::new();
    let walker = WalkBuilder::new(root).standard_filters(true).build();
    for entry in walker.flatten() {
        if hits.len() >= max_hits {
            break;
        }
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if crate::code::Language::from_path(path).is_none() {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        if source.contains(symbol) {
            let rel = path.strip_prefix(root).unwrap_or(path);
            hits.insert(rel.display().to_string());
        }
    }
    Ok(hits.into_iter().collect())
}

fn ripgrep_docs(root: &Path, symbol: &str) -> Result<Vec<String>> {
    let mut hits: BTreeSet<String> = BTreeSet::new();
    let walker = WalkBuilder::new(root).standard_filters(true).build();
    for entry in walker.flatten() {
        if hits.len() >= 10 {
            break;
        }
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if !matches!(ext, "md" | "txt" | "rst") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        if source.contains(symbol) {
            let rel = path.strip_prefix(root).unwrap_or(path);
            hits.insert(rel.display().to_string());
        }
    }
    Ok(hits.into_iter().collect())
}

fn list_workspace_tests_matching(root: &Path, stem: &str) -> Result<Vec<String>> {
    let mut hits: BTreeSet<String> = BTreeSet::new();
    let walker = WalkBuilder::new(root).standard_filters(true).build();
    for entry in walker.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let path_str = path.to_string_lossy().to_string();
        let is_test = path_str.contains("/tests/")
            || path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.ends_with("_test") || s.ends_with("_tests"))
                .unwrap_or(false);
        if !is_test {
            continue;
        }
        if path_str.contains(stem) {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .display()
                .to_string();
            hits.insert(rel);
        }
    }
    Ok(hits.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn write(path: PathBuf, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn fixture_repo() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        write(
            dir.path().join("Cargo.toml"),
            r#"[package]
name = "fixture"
version = "0.1.0"
edition = "2024"
"#,
        );
        write(
            dir.path().join("src/lib.rs"),
            r#"pub fn answer_to_life() -> i32 {
    42
}

pub fn helper_function() -> i32 {
    answer_to_life() + 1
}
"#,
        );
        write(
            dir.path().join("src/consumer.rs"),
            r#"use crate::answer_to_life;

pub fn use_it() {
    let _ = answer_to_life();
}
"#,
        );
        write(
            dir.path().join("tests/lib_tests.rs"),
            r#"#[test]
fn it_works() {
    assert_eq!(fixture::answer_to_life(), 42);
}
"#,
        );
        write(
            dir.path().join("README.md"),
            "## answer_to_life\n\nReturns 42.\n",
        );
        dir
    }

    #[test]
    fn generate_finds_module_test_and_doc_references() {
        let dir = fixture_repo();
        let target = dir.path().join("src/lib.rs");
        let report = generate_for_file(dir.path(), &target).unwrap();
        let modules: Vec<&str> = report
            .affected_modules
            .iter()
            .map(|i| i.path.as_str())
            .collect();
        assert!(
            modules.iter().any(|p| p.contains("consumer.rs")),
            "expected consumer.rs in affected modules, got {modules:?}"
        );
        assert!(
            report
                .affected_tests
                .iter()
                .any(|i| i.path.contains("lib_tests.rs")),
            "expected lib_tests.rs in affected tests, got {:?}",
            report.affected_tests
        );
        assert!(
            report.affected_docs.iter().any(|i| i.path == "README.md"),
            "expected README.md in affected docs, got {:?}",
            report.affected_docs
        );
        // Cargo workspace → cargo verifications.
        let cmds: Vec<&str> = report
            .recommended_verifications
            .iter()
            .map(|v| v.command.as_str())
            .collect();
        assert!(cmds.iter().any(|c| c.starts_with("cargo build")));
        assert!(cmds.iter().any(|c| c.starts_with("cargo test")));
    }

    #[test]
    fn generate_assigns_evidence_when_symbols_extracted() {
        let dir = fixture_repo();
        let target = dir.path().join("src/lib.rs");
        let report = generate_for_file(dir.path(), &target).unwrap();
        let evidence = report
            .affected_modules
            .iter()
            .filter(|i| i.confidence == Confidence::Evidence)
            .count();
        assert!(evidence >= 1, "{:?}", report.affected_modules);
    }

    #[test]
    fn generate_for_symbol_uses_code_graph_edges_and_reference_search() {
        let dir = fixture_repo();
        let graph = crate::code::graph::CodeGraph::open(
            dir.path().to_path_buf(),
            std::sync::Arc::new(crate::code::ParserCache::new()),
        )
        .unwrap();
        graph
            .reindex_with_edges(Some(&[crate::code::graph::CodeGraphEdge {
                kind: crate::code::graph::EdgeKind::Call,
                source_file: "src/consumer.rs".into(),
                source_name: Some("use_it".into()),
                source_start_line: 3,
                source_start_character: 1,
                target_file: "src/lib.rs".into(),
                target_name: Some("answer_to_life".into()),
                target_start_line: 1,
                target_start_character: 1,
                provider: crate::code::graph::EdgeProvider::Lsp,
            }]))
            .unwrap();

        let report = generate_for_symbol(dir.path(), "answer_to_life").unwrap();

        assert_eq!(report.target, "symbol: answer_to_life");
        assert!(
            report.affected_modules.iter().any(|item| {
                item.path == "src/consumer.rs"
                    && item.symbol.as_deref() == Some("use_it")
                    && item.source == ImpactSource::CodeGraph
                    && item.confidence == Confidence::Evidence
            }),
            "expected code graph caller in affected modules, got {:?}",
            report.affected_modules
        );
        assert!(
            report.affected_tests.iter().any(|item| {
                item.path == "tests/lib_tests.rs"
                    && item.source == ImpactSource::RipgrepSearch
                    && item.confidence == Confidence::Evidence
            }),
            "expected textual test reference, got {:?}",
            report.affected_tests
        );
        assert!(
            report
                .rationale
                .as_deref()
                .unwrap_or_default()
                .contains("code graph"),
            "expected rationale to cite code graph/search evidence: {:?}",
            report.rationale
        );
    }

    #[test]
    fn generate_for_symbol_falls_back_when_index_is_partial() {
        let dir = fixture_repo();
        let graph = crate::code::graph::CodeGraph::open(
            dir.path().to_path_buf(),
            std::sync::Arc::new(crate::code::ParserCache::new()),
        )
        .unwrap();
        graph.reindex_with_edges(None).unwrap();

        let report = generate_for_symbol(dir.path(), "answer_to_life").unwrap();

        assert!(
            report
                .affected_modules
                .iter()
                .any(|item| item.path == "src/consumer.rs"
                    && item.source == ImpactSource::References),
            "expected reference-search fallback, got {:?}",
            report.affected_modules
        );
        assert!(
            report
                .rationale
                .as_deref()
                .unwrap_or_default()
                .contains("partial or unavailable"),
            "expected partial-index rationale, got {:?}",
            report.rationale
        );
    }

    #[test]
    fn generate_falls_back_to_medium_risk_for_unparseable_target() {
        // A non-source file (no language match) yields zero
        // symbols → Medium risk.
        let dir = tempdir().unwrap();
        write(
            dir.path().join("Cargo.toml"),
            "[package]\nname=\"fixture\"\nversion=\"0.1.0\"\n",
        );
        write(dir.path().join("data.bin"), "\x00\x01\x02");
        let target = dir.path().join("data.bin");
        let report = generate_for_file(dir.path(), &target).unwrap();
        assert_eq!(report.risk, Some(RiskLevel::Medium));
    }

    #[test]
    fn cargo_verifications_are_skipped_outside_a_cargo_workspace() {
        let dir = tempdir().unwrap();
        write(dir.path().join("hello.py"), "def f():\n    return 1\n");
        let report = generate_for_file(dir.path(), &dir.path().join("hello.py")).unwrap();
        assert!(report.recommended_verifications.is_empty());
    }

    #[test]
    fn risk_low_when_fanout_small() {
        assert_eq!(infer_risk(1, 1, true), RiskLevel::Low);
        assert_eq!(infer_risk(3, 1, true), RiskLevel::Low);
    }

    #[test]
    fn risk_medium_when_fanout_moderate() {
        assert_eq!(infer_risk(4, 1, true), RiskLevel::Medium);
        assert_eq!(infer_risk(10, 0, true), RiskLevel::Medium);
    }

    #[test]
    fn risk_high_when_fanout_large() {
        assert_eq!(infer_risk(15, 5, true), RiskLevel::High);
    }
}
