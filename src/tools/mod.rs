use crate::provider::ToolDefinition;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Canonical tool return type — the wire format between `Tool::call`, the
/// agent loop, and `AfterToolCall` hooks.
///
/// `output` goes to the LLM (via `Message::Tool` content). `media` carries
/// file paths for attachments (images, audio, etc.) — the agent serializes
/// them inline as `[media: ...]` markers when flattening for the message
/// payload, but hooks and the TUI see them as a separate field. `is_error`
/// is the structured signal that something went wrong (preferred over
/// string-prefix sniffing like `output.starts_with("Error:")`).
#[derive(Debug, Clone, Default)]
pub struct ToolResult {
    pub output: String,
    pub media: Vec<String>,
    pub is_error: bool,
    /// Optional richer body the TUI uses for the YYC-74 card preview.
    /// Lets file-edit tools render an actual diff (`+ ... / - ...`)
    /// inside the card while the LLM-facing `output` stays terse
    /// (`Replaced 1 occurrence(s) in foo.rs`). Plain-output tools
    /// leave this `None` and the renderer falls back to `output`.
    pub display_preview: Option<String>,
    /// Per-call diff record from a file-editing tool (YYC-131). Travels
    /// with the result through the dispatch pipeline so AfterToolCall
    /// hooks (e.g. DiagnosticsHook) can see *this* call's diff rather
    /// than racing on a global last-write slot under concurrent
    /// dispatch.
    pub edit_diff: Option<EditDiff>,
}

impl ToolResult {
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            media: Vec::new(),
            is_error: false,
            display_preview: None,
            edit_diff: None,
        }
    }

    pub fn err(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            media: Vec::new(),
            is_error: true,
            display_preview: None,
            edit_diff: None,
        }
    }

    pub fn with_display_preview(mut self, preview: impl Into<String>) -> Self {
        self.display_preview = Some(preview.into());
        self
    }

    /// Attach the per-call edit diff so AfterToolCall hooks can react
    /// to *this* call without racing on the global EditDiffSink slot
    /// (YYC-131).
    pub fn with_edit_diff(mut self, diff: EditDiff) -> Self {
        self.edit_diff = Some(diff);
        self
    }
}

impl From<String> for ToolResult {
    fn from(output: String) -> Self {
        Self::ok(output)
    }
}

impl From<&str> for ToolResult {
    fn from(output: &str) -> Self {
        Self::ok(output)
    }
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> Value;
    /// Execute the tool. The `cancel` token fires when the user requests
    /// cancellation; impls should race their work against `cancel.cancelled()`
    /// (or rely on `kill_on_drop` for child processes) and return
    /// `ToolResult::err("Cancelled")` on cancel.
    async fn call(&self, params: Value, cancel: CancellationToken) -> Result<ToolResult>;

    /// YYC-107: per-session availability check. Default = always
    /// register. Tools that only make sense in certain workspaces
    /// (cargo_check needs Cargo.toml, etc.) override this.
    fn is_relevant(&self, _ctx: &ToolContext) -> bool {
        true
    }

    /// YYC-107: optional runtime-aware description. Returns
    /// `Some(string)` to override the static description with
    /// workspace-derived context (e.g. discovered bin targets).
    fn dynamic_description(&self, _ctx: &ToolContext) -> Option<String> {
        None
    }
}

/// Workspace context surfaced to tools at session start (YYC-107).
/// Built once by `AgentBuilder::build` and passed into
/// `is_relevant` / `dynamic_description` so tools can reflect what's
/// actually in the cwd.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub cwd: std::path::PathBuf,
    /// First Cargo.toml found within `MAX_CONTEXT_DEPTH` of cwd.
    pub cargo_manifest: Option<std::path::PathBuf>,
    /// Parsed package name from the discovered Cargo.toml, when readable.
    pub cargo_package_name: Option<String>,
    /// Parsed `[[bin]]` target names from the discovered Cargo.toml.
    pub cargo_bin_targets: Vec<String>,
    /// True when the cwd is inside a git working tree.
    pub git_present: bool,
}

const MAX_CONTEXT_DEPTH: usize = 4;

impl ToolContext {
    /// Build the context by probing the filesystem rooted at `cwd`.
    pub fn probe(cwd: std::path::PathBuf) -> Self {
        let cargo_manifest = find_cargo_manifest(&cwd, MAX_CONTEXT_DEPTH);
        let (cargo_package_name, cargo_bin_targets) = match &cargo_manifest {
            Some(path) => parse_cargo_manifest(path),
            None => (None, Vec::new()),
        };
        let git_present = is_git_workspace(&cwd);
        Self {
            cwd,
            cargo_manifest,
            cargo_package_name,
            cargo_bin_targets,
            git_present,
        }
    }
}

fn find_cargo_manifest(start: &std::path::Path, max_depth: usize) -> Option<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, depth_left: usize) -> Option<std::path::PathBuf> {
        let candidate = dir.join("Cargo.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        if depth_left == 0 {
            return None;
        }
        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            // Skip hidden and target/ to keep the probe fast.
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            if let Some(found) = walk(&path, depth_left - 1) {
                return Some(found);
            }
        }
        None
    }
    walk(start, max_depth)
}

fn parse_cargo_manifest(path: &std::path::Path) -> (Option<String>, Vec<String>) {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return (None, Vec::new()),
    };
    let parsed: toml::Value = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return (None, Vec::new()),
    };
    let pkg_name = parsed
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string());
    let bins: Vec<String> = parsed
        .get("bin")
        .and_then(|b| b.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|b| {
                    b.get("name")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    (pkg_name, bins)
}

fn is_git_workspace(start: &std::path::Path) -> bool {
    let mut cur = Some(start);
    while let Some(p) = cur {
        if p.join(".git").exists() {
            return true;
        }
        cur = p.parent();
    }
    false
}

pub mod ask_user;
pub mod cargo;
pub mod code;
pub mod code_edit;
pub mod code_graph;
pub mod code_search;
pub mod file;
pub mod fs_sandbox;
pub mod git;
pub mod lsp;
pub mod profile;
pub mod shell;
pub mod spawn;
pub mod web;
pub mod web_ssrf;

pub use profile::{ToolProfile, builtin_profile, builtin_profiles};

/// Compact record of the most recent file-edit operation (YYC-66).
/// Captured by `WriteFile`/`PatchFile` after a successful write so the
/// TUI's diff pane can render real activity instead of demo data.
#[derive(Debug, Clone)]
pub struct EditDiff {
    pub path: String,
    /// Tool that produced the edit ("write_file" / "edit_file").
    pub tool: String,
    /// Snippet of the file contents *before* the edit. Empty for
    /// freshly-created files.
    pub before: String,
    /// Snippet of the file contents *after* the edit.
    pub after: String,
    pub at: chrono::DateTime<chrono::Local>,
}

/// Shared latest-edit slot. `None` until the first successful edit;
/// overwritten on every subsequent edit. The TUI clones the Arc and
/// peeks the inner Option each render.
pub type EditDiffSink = Arc<parking_lot::Mutex<Option<EditDiff>>>;

pub fn new_diff_sink() -> EditDiffSink {
    Arc::new(parking_lot::Mutex::new(None))
}

/// Trim a string to a max number of lines + chars so the TUI doesn't
/// stash megabyte-sized files in memory just to render a 6-line preview.
pub(crate) fn snippet(text: &str, max_lines: usize, max_chars: usize) -> String {
    let limited: String = text.chars().take(max_chars).collect();
    limited
        .lines()
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Registry of available tools — tools are discovered at startup via the `inventory` pattern
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    /// YYC-181: name of the active capability profile, when one was
    /// applied via [`Self::apply_profile`]. `None` means the registry
    /// is unrestricted (the historical default).
    active_profile: Option<String>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::new_with_diff_sink(None)
    }

    /// Build a tool registry that wires `WriteFile`/`PatchFile` to a
    /// shared diff sink (YYC-66). Pass `Some(sink)` to capture edits;
    /// `None` keeps the legacy behavior (tools don't observe their own
    /// writes). LSP tools are registered with their own manager so
    /// callers don't have to thread it separately (YYC-46).
    ///
    /// Probes `current_dir()` for the LSP manager and code graph. Use
    /// [`Self::new_with_diff_and_lsp`] when the caller already has a
    /// resolved cwd to avoid duplicate probes (YYC-116).
    pub fn new_with_diff_sink(sink: Option<EditDiffSink>) -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        Self::new_with_diff_and_lsp(sink, None, cwd)
    }

    /// Same as `new_with_diff_sink`, plus an external LSP manager so
    /// the agent can share one across tools and the diagnostics hook
    /// (YYC-51). When `lsp` is `None`, a per-registry manager is
    /// created rooted at `cwd`. Pass the cwd that's already been
    /// probed at the call site rather than re-probing here (YYC-116).
    pub fn new_with_diff_and_lsp(
        sink: Option<EditDiffSink>,
        lsp: Option<Arc<crate::code::lsp::LspManager>>,
        cwd: std::path::PathBuf,
    ) -> Self {
        let mut registry = Self {
            tools: HashMap::new(),
            active_profile: None,
        };
        registry.register(Arc::new(file::ReadFile));
        registry.register(Arc::new(file::WriteFile::new(sink.clone())));
        registry.register(Arc::new(file::SearchFiles));
        registry.register(Arc::new(file::PatchFile::new(sink)));
        // YYC-79: native tree listing.
        registry.register(Arc::new(file::ListFiles));
        // YYC-80: structured Rust compile diagnostics.
        registry.register(Arc::new(cargo::CargoCheckTool));
        registry.register(Arc::new(web::WebSearch));
        registry.register(Arc::new(web::WebFetch));
        // YYC-45: tree-sitter structural code tools. One parser cache
        // shared across all three so we only initialize each grammar
        // once per session.
        let parser_cache = Arc::new(crate::code::ParserCache::new());
        registry.register(Arc::new(code::CodeOutlineTool::new(parser_cache.clone())));
        registry.register(Arc::new(code::CodeExtractTool::new(parser_cache.clone())));
        registry.register(Arc::new(code::CodeQueryTool::new(parser_cache.clone())));
        // YYC-46: LSP-backed semantic tools. One manager pool — servers
        // are spawned lazily on first use per language.
        let lsp_mgr =
            lsp.unwrap_or_else(|| Arc::new(crate::code::lsp::LspManager::new(cwd.clone())));
        registry.register(Arc::new(lsp::GotoDefinitionTool::new(lsp_mgr.clone())));
        registry.register(Arc::new(lsp::FindReferencesTool::new(lsp_mgr.clone())));
        registry.register(Arc::new(lsp::HoverTool::new(lsp_mgr.clone())));
        registry.register(Arc::new(lsp::DiagnosticsTool::new(lsp_mgr.clone())));
        // YYC-201: workspace-wide symbol search via LSP.
        registry.register(Arc::new(lsp::WorkspaceSymbolTool::new(lsp_mgr.clone())));
        // YYC-202: type-of-expression + trait/interface implementation lookup.
        registry.register(Arc::new(lsp::TypeDefinitionTool::new(lsp_mgr.clone())));
        registry.register(Arc::new(lsp::ImplementationTool::new(lsp_mgr.clone())));
        // YYC-203: incoming/outgoing call hierarchy.
        registry.register(Arc::new(lsp::CallHierarchyTool::new(lsp_mgr.clone())));
        // YYC-204: code actions (fix-its + refactors).
        registry.register(Arc::new(lsp::CodeActionTool::new(lsp_mgr.clone())));
        // YYC-49: AST-aware structural edits.
        registry.register(Arc::new(code_edit::ReplaceFunctionBodyTool));
        registry.register(Arc::new(code_edit::RenameSymbolTool::new(lsp_mgr)));
        // YYC-50: workspace symbol index. Lazy — the agent has to run
        // `index_code_graph` once before `find_symbol` returns hits.
        if let Ok(graph) = crate::code::graph::CodeGraph::open(cwd, parser_cache.clone()) {
            let graph_arc = Arc::new(graph);
            registry.register(Arc::new(code_graph::IndexCodeGraphTool::new(
                graph_arc.clone(),
            )));
            registry.register(Arc::new(code_graph::FindSymbolTool::new(graph_arc)));
        }
        // YYC-36: native git tools — agent stops composing brittle
        // `git ...` shell strings through bash.
        registry.register(Arc::new(git::GitStatusTool));
        registry.register(Arc::new(git::GitDiffTool));
        registry.register(Arc::new(git::GitCommitTool));
        registry.register(Arc::new(git::GitPushTool));
        registry.register(Arc::new(git::GitBranchTool));
        registry.register(Arc::new(git::GitLogTool));
        for tool in shell::make_tools() {
            registry.register(tool);
        }
        registry
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// YYC-181: apply a named tool capability profile to this
    /// registry. Drops every tool not in `profile.allowed` and
    /// records `profile.name` so callers (run records, doctor,
    /// subagent inheritance) can read what's active without
    /// re-deriving the set.
    pub fn apply_profile(&mut self, profile: &ToolProfile) {
        let allowed: Vec<String> = profile.allowed.iter().map(|s| s.to_string()).collect();
        self.retain_only(&allowed);
        self.active_profile = Some(profile.name.to_string());
    }

    /// YYC-181: name of the currently active capability profile, or
    /// `None` if the registry is unrestricted.
    pub fn active_profile(&self) -> Option<&str> {
        self.active_profile.as_deref()
    }

    /// YYC-82: keep only tools whose name appears in `allowed`.
    /// Used by `spawn_subagent` to scope the child agent's tool
    /// access. Tools not present in the parent registry are
    /// silently ignored — the caller can reconcile by checking
    /// `definitions()` afterwards if it cares.
    pub fn retain_only(&mut self, allowed: &[String]) {
        let drop_keys: Vec<String> = self
            .tools
            .keys()
            .filter(|k| !allowed.iter().any(|a| a == *k))
            .cloned()
            .collect();
        for k in drop_keys {
            self.tools.remove(&k);
        }
    }

    /// YYC-107: drop tools whose `is_relevant(ctx)` returns false.
    /// Called once at session start by `AgentBuilder::build`
    /// after the registry is fully populated.
    pub fn filter_for_context(&mut self, ctx: &ToolContext) {
        let drop_keys: Vec<String> = self
            .tools
            .iter()
            .filter_map(|(name, tool)| {
                if tool.is_relevant(ctx) {
                    None
                } else {
                    Some(name.clone())
                }
            })
            .collect();
        for k in drop_keys {
            self.tools.remove(&k);
        }
    }

    /// Get all tool definitions for the LLM. Tools that override
    /// `dynamic_description` get their runtime description; the rest
    /// fall back to the static one.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.definitions_with_context(None)
    }

    pub fn definitions_with_context(&self, ctx: Option<&ToolContext>) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|t| {
                let description = ctx
                    .and_then(|c| t.dynamic_description(c))
                    .unwrap_or_else(|| t.description().to_string());
                ToolDefinition {
                    tool_type: "function".into(),
                    function: crate::provider::ToolFunction {
                        name: t.name().to_string(),
                        description,
                        parameters: t.schema(),
                    },
                }
            })
            .collect()
    }

    /// Execute a tool by name with JSON arguments.
    pub async fn execute(
        &self,
        name: &str,
        arguments: &str,
        cancel: CancellationToken,
    ) -> Result<ToolResult> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Unknown tool: {name}"))?;

        let params: Value = serde_json::from_str(arguments).map_err(|e| {
            // Include the raw args so the LLM can see what it generated and
            // self-correct on the next turn rather than hallucinating fixes.
            anyhow::anyhow!("Failed to parse arguments for {name}: {e}. Raw args: {arguments}")
        })?;

        // Lightweight schema validation: check required fields are present
        // before dispatch. Catches the common "model forgot a required arg"
        // failure mode early with a clear error containing the schema, so
        // the agent can self-correct on the next turn (YYC-39).
        let schema = tool.schema();
        validate_tool_params(name, &schema, &params, arguments)?;

        tool.call(params, cancel).await
    }
}

/// Returns a comma-separated list of `required` schema fields that are
/// missing from `params`, or `None` if all required fields are present (or
/// the schema doesn't declare any).
fn missing_required_fields(schema: &Value, params: &Value) -> Option<String> {
    let required = schema.get("required")?.as_array()?;
    let provided = params.as_object()?;
    let missing: Vec<&str> = required
        .iter()
        .filter_map(|v| v.as_str())
        .filter(|key| !provided.contains_key(*key))
        .collect();
    if missing.is_empty() {
        None
    } else {
        Some(missing.join(", "))
    }
}

fn validate_tool_params(
    name: &str,
    schema: &Value,
    params: &Value,
    raw_arguments: &str,
) -> Result<()> {
    let schema_text =
        serde_json::to_string(schema).unwrap_or_else(|_| "<unserializable schema>".into());

    if params.as_object().is_none() {
        anyhow::bail!(
            "Tool '{name}' arguments must be a JSON object. Schema: {schema_text}. \
             You provided: {raw_arguments}"
        );
    }

    if let Some(missing) = missing_required_fields(schema, params) {
        let required = schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        anyhow::bail!(
            "Tool '{name}' is missing required field(s): {missing}. \
             Required fields are: [{required}]. Schema: {schema_text}. \
             You provided: {raw_arguments}"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn tool_context_finds_cargo_manifest_at_root_and_nested() {
        let dir = tempdir().unwrap();
        // Empty dir → no manifest.
        let ctx = ToolContext::probe(dir.path().to_path_buf());
        assert!(ctx.cargo_manifest.is_none());

        // Cargo.toml at root.
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let ctx = ToolContext::probe(dir.path().to_path_buf());
        assert!(ctx.cargo_manifest.is_some());
        assert_eq!(ctx.cargo_package_name.as_deref(), Some("demo"));

        // Nested Cargo.toml is found within depth.
        let nested_dir = tempdir().unwrap();
        let sub = nested_dir.path().join("crate").join("nested");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            sub.join("Cargo.toml"),
            "[package]\nname = \"deep\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let ctx = ToolContext::probe(nested_dir.path().to_path_buf());
        assert_eq!(ctx.cargo_package_name.as_deref(), Some("deep"));
    }

    #[test]
    fn tool_context_parses_bin_targets() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            r#"
[package]
name = "demo"
version = "0.1.0"

[[bin]]
name = "alpha"
path = "src/alpha.rs"

[[bin]]
name = "beta"
path = "src/beta.rs"
"#,
        )
        .unwrap();
        let ctx = ToolContext::probe(dir.path().to_path_buf());
        let mut bins = ctx.cargo_bin_targets.clone();
        bins.sort();
        assert_eq!(bins, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn cargo_check_is_filtered_when_no_manifest() {
        // Probe an empty dir → no manifest → CargoCheckTool::is_relevant false.
        let dir = tempdir().unwrap();
        let ctx = ToolContext::probe(dir.path().to_path_buf());
        assert!(!cargo::CargoCheckTool.is_relevant(&ctx));
    }

    #[test]
    fn cargo_check_dynamic_description_lists_targets() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            r#"
[package]
name = "demo"
version = "0.1.0"

[[bin]]
name = "primary"
path = "src/primary.rs"
"#,
        )
        .unwrap();
        let ctx = ToolContext::probe(dir.path().to_path_buf());
        let dyn_desc = cargo::CargoCheckTool.dynamic_description(&ctx).unwrap();
        assert!(dyn_desc.contains("`demo`"));
        assert!(dyn_desc.contains("primary"));
    }

    #[tokio::test]
    async fn missing_required_field_yields_clear_error() {
        let registry = ToolRegistry::new();
        // edit_file requires path, old_string, new_string. Omit new_string.
        let bogus_args = json!({
            "path": "/tmp/x",
            "old_string": "foo"
            // new_string missing
        })
        .to_string();

        let err = registry
            .execute("edit_file", &bogus_args, CancellationToken::new())
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("missing required"), "got {msg:?}");
        assert!(msg.contains("new_string"), "got {msg:?}");
        assert!(msg.contains("Required fields"), "got {msg:?}");
        assert!(msg.contains("Schema"), "got {msg:?}");
    }

    #[tokio::test]
    async fn malformed_json_yields_clear_error() {
        let registry = ToolRegistry::new();
        let err = registry
            .execute("read_file", "{not valid json", CancellationToken::new())
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Failed to parse arguments"), "got {msg:?}");
        assert!(msg.contains("Raw args"), "got {msg:?}");
    }

    #[test]
    fn registry_accepts_explicit_cwd_without_probing() {
        // YYC-116: callers that already have a resolved cwd pass it
        // through; the registry must honor it without falling back to
        // current_dir(). A non-existent path is the cleanest signal —
        // construction succeeds (the code graph open just no-ops on
        // failure) and the registry contains the expected core tools.
        let bogus = std::path::PathBuf::from("/nonexistent/yyc-116/probe-root");
        let registry = ToolRegistry::new_with_diff_and_lsp(None, None, bogus);
        let defs = registry.definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        for must in ["read_file", "goto_definition", "git_status"] {
            assert!(
                names.contains(&must),
                "registry missing {must:?}; have {names:?}"
            );
        }
    }

    #[test]
    fn missing_required_handles_empty_or_absent_required() {
        // Schema with no `required` key — should pass.
        let schema = json!({"type": "object", "properties": {}});
        assert!(missing_required_fields(&schema, &json!({})).is_none());
        // Schema with empty required array — should pass.
        let schema = json!({"type": "object", "required": []});
        assert!(missing_required_fields(&schema, &json!({})).is_none());
        // Required, all present.
        let schema = json!({"required": ["a", "b"]});
        assert!(missing_required_fields(&schema, &json!({"a": 1, "b": 2})).is_none());
        // Required, one missing.
        let schema = json!({"required": ["a", "b"]});
        let missing = missing_required_fields(&schema, &json!({"a": 1})).unwrap();
        assert_eq!(missing, "b");
    }

    /// YYC-85: every native tool that has a clear bash equivalent must
    /// state it in its description, so the model sees the redirect on
    /// every turn (descriptions ship in the tool spec). Failing here
    /// means a tool was added without the "instead of" hint.
    #[test]
    fn native_tool_descriptions_call_out_bash_equivalents() {
        let registry = ToolRegistry::new();
        let must_redirect = [
            "read_file",
            "write_file",
            "list_files",
            "search_files",
            "edit_file",
            "cargo_check",
            "code_outline",
            "code_query",
            "code_extract",
            "git_status",
            "git_diff",
            "git_commit",
            "git_push",
            "git_branch",
            "git_log",
        ];
        let defs = registry.definitions();
        for name in must_redirect {
            let def = defs
                .iter()
                .find(|d| d.function.name == name)
                .unwrap_or_else(|| panic!("expected `{name}` in registry"));
            let desc = &def.function.description;
            assert!(
                desc.contains("instead of"),
                "{name} description missing 'instead of <bash>' clause: {desc:?}"
            );
        }
    }

    // ── YYC-181: tool capability profiles ───────────────────────────

    #[test]
    fn apply_readonly_profile_drops_mutating_tools() {
        let mut registry = ToolRegistry::new();
        let profile = builtin_profile("readonly").expect("readonly profile exists");
        registry.apply_profile(&profile);

        let names: Vec<String> = registry
            .definitions()
            .into_iter()
            .map(|d| d.function.name)
            .collect();
        // Mutating tools must be gone.
        for forbidden in ["write_file", "edit_file", "bash", "git_commit", "git_push"] {
            assert!(
                !names.contains(&forbidden.to_string()),
                "readonly profile should have dropped {forbidden:?}, got {names:?}"
            );
        }
        // Read-only navigators should remain.
        for must in ["read_file", "git_status"] {
            assert!(
                names.contains(&must.to_string()),
                "readonly profile dropped {must:?}; got {names:?}"
            );
        }
        assert_eq!(registry.active_profile(), Some("readonly"));
    }

    #[test]
    fn apply_gateway_safe_profile_blocks_workspace_mutation() {
        let mut registry = ToolRegistry::new();
        let profile = builtin_profile("gateway-safe").expect("profile exists");
        registry.apply_profile(&profile);
        let names: Vec<String> = registry
            .definitions()
            .into_iter()
            .map(|d| d.function.name)
            .collect();
        for forbidden in [
            "write_file",
            "edit_file",
            "bash",
            "cargo_check",
            "git_commit",
        ] {
            assert!(
                !names.contains(&forbidden.to_string()),
                "gateway-safe should have dropped {forbidden:?}; got {names:?}"
            );
        }
    }

    #[tokio::test]
    async fn disallowed_tool_after_profile_returns_unknown_tool_error() {
        // YYC-181 acceptance: calling a disallowed tool surfaces a
        // structured error path (the registry doesn't know about it),
        // not an execution-time surprise from the tool itself.
        let mut registry = ToolRegistry::new();
        let profile = builtin_profile("readonly").unwrap();
        registry.apply_profile(&profile);
        let err = registry
            .execute("write_file", "{}", CancellationToken::new())
            .await
            .expect_err("write_file must be denied under readonly");
        let msg = err.to_string();
        assert!(
            msg.contains("Unknown tool: write_file"),
            "expected Unknown tool error, got {msg:?}"
        );
    }

    #[tokio::test]
    async fn non_object_arguments_fail_before_tool_dispatch() {
        let registry = ToolRegistry::new();
        let err = registry
            .execute("read_file", "[]", CancellationToken::new())
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("JSON object"), "got {msg:?}");
        assert!(msg.contains("Schema"), "got {msg:?}");
    }
}
