//! Auto-diagnostics AfterToolCall hook (YYC-51).
//!
//! After a successful `edit_file` / `write_file`, asks the LSP for the
//! current diagnostics on the touched file and appends an addendum to
//! the tool result if there are errors or warnings the agent should
//! react to. Closes the "did my edit compile?" loop without the agent
//! having to remember to call `diagnostics` itself.
//!
//! The path is recovered from the shared `EditDiffSink` (already
//! populated by WriteFile/PatchFile per YYC-66) so we don't have to
//! parse it back out of the result string.
//!
//! When LSP isn't available for the language (or no server is
//! installed), the hook is a no-op — the agent loses no functionality
//! and there's no spurious error.

use crate::code::Language;
use crate::code::lsp::{LspManager, diagnostics_for};
use crate::hooks::{HookHandler, HookOutcome};
use crate::tools::{EditDiffSink, ToolResult};
use anyhow::Result;
use lsp_types::DiagnosticSeverity;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const MAX_DIAGNOSTICS: usize = 10;

pub struct DiagnosticsHook {
    lsp: Arc<LspManager>,
    diff_sink: EditDiffSink,
}

impl DiagnosticsHook {
    pub fn new(lsp: Arc<LspManager>, diff_sink: EditDiffSink) -> Self {
        Self { lsp, diff_sink }
    }
}

#[async_trait::async_trait]
impl HookHandler for DiagnosticsHook {
    fn name(&self) -> &str {
        "diagnostics_after_edit"
    }

    fn priority(&self) -> i32 {
        // Run late so any other AfterToolCall hooks see the original
        // result first; the diagnostics addendum is purely additive.
        90
    }

    async fn after_tool_call(
        &self,
        tool: &str,
        result: &ToolResult,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        if !matches!(tool, "edit_file" | "write_file") {
            return Ok(HookOutcome::Continue);
        }
        // The edit didn't happen — don't run diagnostics on a write
        // that errored.
        if result.is_error {
            return Ok(HookOutcome::Continue);
        }

        let edit = match self.diff_sink.lock().unwrap().clone() {
            Some(d) => d,
            None => return Ok(HookOutcome::Continue),
        };
        // Defense against stale sink contents — only act if the latest
        // sink entry was produced by the tool we just observed.
        if edit.tool != tool {
            return Ok(HookOutcome::Continue);
        }
        let path = PathBuf::from(&edit.path);
        let lang = match Language::from_path(&path) {
            Some(l) => l,
            None => return Ok(HookOutcome::Continue),
        };

        // Spawn / reuse the LSP server. Soft-fail: if the binary isn't
        // installed, silently skip — the user shouldn't see a noisy
        // hook error every edit.
        let server = match self.lsp.server(lang).await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("diagnostics hook: LSP unavailable for {} ({e})", lang.name());
                return Ok(HookOutcome::Continue);
            }
        };

        let diagnostics = match diagnostics_for(&server, &path).await {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!("diagnostics hook: query failed: {e}");
                return Ok(HookOutcome::Continue);
            }
        };

        // Filter to errors + warnings; sort by severity then line; cap.
        let mut significant: Vec<_> = diagnostics
            .into_iter()
            .filter(|d| {
                matches!(
                    d.severity,
                    Some(DiagnosticSeverity::ERROR) | Some(DiagnosticSeverity::WARNING)
                )
            })
            .collect();
        if significant.is_empty() {
            // Silence is information — no addendum.
            return Ok(HookOutcome::Continue);
        }
        significant.sort_by_key(|d| {
            (
                severity_rank(d.severity),
                d.range.start.line,
                d.range.start.character,
            )
        });
        significant.truncate(MAX_DIAGNOSTICS);

        // Build the addendum and replace the result.
        let mut body = String::from("\n\n--- diagnostics after edit ---\n");
        for d in &significant {
            let line = d.range.start.line + 1;
            let col = d.range.start.character + 1;
            let sev = severity_label(d.severity);
            body.push_str(&format!(
                "{}:{}:{} {}: {}\n",
                edit.path,
                line,
                col,
                sev,
                d.message.trim()
            ));
        }
        let mut combined = result.clone();
        combined.output.push_str(&body);
        Ok(HookOutcome::ReplaceResult(combined))
    }
}

fn severity_rank(s: Option<DiagnosticSeverity>) -> u8 {
    match s {
        Some(DiagnosticSeverity::ERROR) => 0,
        Some(DiagnosticSeverity::WARNING) => 1,
        Some(DiagnosticSeverity::INFORMATION) => 2,
        Some(DiagnosticSeverity::HINT) => 3,
        _ => 4,
    }
}

fn severity_label(s: Option<DiagnosticSeverity>) -> &'static str {
    match s {
        Some(DiagnosticSeverity::ERROR) => "error",
        Some(DiagnosticSeverity::WARNING) => "warning",
        Some(DiagnosticSeverity::INFORMATION) => "info",
        Some(DiagnosticSeverity::HINT) => "hint",
        _ => "note",
    }
}

#[allow(dead_code)]
fn _silence_value_warning(_: Value) {} // Reserved if future signature work needs Value.
