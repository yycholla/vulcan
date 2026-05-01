//! `cargo_check` tool (YYC-80).
//!
//! Runs `cargo check --message-format=json` in the workspace and emits
//! a structured array of compiler diagnostics. Complements the LSP
//! `diagnostics` tool (YYC-46): LSP needs rust-analyzer running; this
//! works cold on any Rust project. Pairs naturally with the YYC-51
//! auto-diagnostics hook for a "did my edit compile?" Rust path that
//! doesn't depend on tooling state.

use crate::tools::{Tool, ToolContext, ToolResult, parse_tool_params};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

const MAX_DIAGS: usize = 50;

fn default_cargo_all_targets() -> bool {
    true
}

#[derive(Deserialize)]
struct CargoCheckParams {
    #[serde(default)]
    package: Option<String>,
    #[serde(default = "default_cargo_all_targets")]
    all_targets: bool,
}

pub struct CargoCheckTool;

#[async_trait]
impl Tool for CargoCheckTool {
    fn name(&self) -> &str {
        "cargo_check"
    }
    fn description(&self) -> &str {
        "Run `cargo check --message-format=json` in the cwd and return the parsed compiler diagnostics. Works without rust-analyzer indexed; complements the LSP `diagnostics` tool. Use this instead of `cargo check` or `cargo build` via bash — structured errors with paths, lines, and rendered messages."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "package": {
                    "type": "string",
                    "description": "Limit to a specific workspace member (defaults to --workspace)"
                },
                "all_targets": {
                    "type": "boolean",
                    "description": "Include tests + examples (default true)",
                    "default": true
                }
            }
        })
    }
    fn is_relevant(&self, ctx: &ToolContext) -> bool {
        // YYC-107: only register cargo_check when there's a Cargo.toml
        // within the probe depth. Saves the agent from the confusing
        // "could not find Cargo.toml" error path on non-Rust workspaces.
        ctx.cargo_manifest.is_some()
    }

    fn dynamic_description(&self, ctx: &ToolContext) -> Option<String> {
        let manifest = ctx.cargo_manifest.as_ref()?;
        let mut out = String::from(
            "Run `cargo check --message-format=json` and return parsed compiler diagnostics. \
             Use this instead of `cargo check` via bash — structured errors with paths, lines, \
             rendered messages.",
        );
        if let Some(name) = &ctx.cargo_package_name {
            out.push_str(&format!(" Workspace package: `{name}`."));
        }
        if !ctx.cargo_bin_targets.is_empty() {
            out.push_str(&format!(
                " Binary targets: {}.",
                ctx.cargo_bin_targets.join(", ")
            ));
        }
        out.push_str(&format!(" Manifest: `{}`.", manifest.display()));
        Some(out)
    }

    async fn call(
        &self,
        params: Value,
        cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: CargoCheckParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let package = p.package.as_deref();
        let all_targets = p.all_targets;

        let mut cmd = Command::new("cargo");
        cmd.arg("check");
        cmd.arg("--message-format=json");
        if let Some(pkg) = package {
            cmd.arg("-p").arg(pkg);
        } else {
            cmd.arg("--workspace");
        }
        if all_targets {
            cmd.arg("--all-targets");
        }
        cmd.kill_on_drop(true);

        let output = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(ToolResult::err("Cancelled")),
            r = cmd.output() => r?,
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut diagnostics: Vec<Value> = Vec::new();
        for line in stdout.lines() {
            if line.is_empty() {
                continue;
            }
            let raw: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if raw.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
                continue;
            }
            let msg = match raw.get("message") {
                Some(m) => m,
                None => continue,
            };
            let level = msg.get("level").and_then(|v| v.as_str()).unwrap_or("note");
            // Skip purely informational rustc notes (`level=note` only),
            // keep error/warning/help so the agent sees actionable output.
            if level == "note" {
                continue;
            }
            let span = msg
                .get("spans")
                .and_then(|v| v.as_array())
                .and_then(|a| a.iter().find(|s| s.get("is_primary") == Some(&json!(true))))
                .or_else(|| {
                    msg.get("spans")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                });
            let (file, line, col) = match span {
                Some(s) => (
                    s.get("file_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    s.get("line_start").and_then(|v| v.as_u64()).unwrap_or(0),
                    s.get("column_start").and_then(|v| v.as_u64()).unwrap_or(0),
                ),
                None => (String::new(), 0, 0),
            };
            let code = msg
                .get("code")
                .and_then(|c| c.get("code"))
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            let message = msg
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            diagnostics.push(json!({
                "file": file,
                "line": line,
                "col": col,
                "level": level,
                "code": code,
                "message": message,
            }));
            if diagnostics.len() >= MAX_DIAGS {
                break;
            }
        }

        // Sort by severity (error first), then file/line.
        diagnostics.sort_by_key(|d| {
            let sev = match d["level"].as_str().unwrap_or("") {
                "error" => 0,
                "warning" => 1,
                _ => 2,
            };
            (sev, d["line"].as_u64().unwrap_or(0))
        });

        let payload = json!({
            "ok": output.status.success(),
            "exit_code": output.status.code().unwrap_or(-1),
            "count": diagnostics.len(),
            "diagnostics": diagnostics,
            // Fall back stderr only on non-success so noisy compile
            // summaries don't pad the response on success.
            "stderr": if output.status.success() {
                Value::Null
            } else {
                Value::String(stderr.to_string())
            },
        });
        Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
    }
}

#[cfg(test)]
mod yyc263_tests {
    use super::*;

    #[tokio::test]
    async fn cargo_check_bad_param_type_surfaces_as_toolresult_err() {
        let result = CargoCheckTool
            .call(
                json!({ "all_targets": "yes" }),
                CancellationToken::new(),
                None,
            )
            .await
            .expect("call returns Ok(ToolResult)");
        assert!(result.is_error);
        assert!(
            result.output.contains("tool params failed to validate"),
            "expected serde-shaped error, got: {}",
            result.output
        );
    }
}
