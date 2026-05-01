use crate::provider::ToolDefinition;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolResult {
    pub output: String,
    pub media: Vec<String>,
    pub is_error: bool,
    /// Extension-owned structured state carried alongside the
    /// LLM-facing output. Session extensions can replay this from
    /// persisted tool messages when reconstructing per-session state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
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
            details: None,
            display_preview: None,
            edit_diff: None,
        }
    }

    pub fn err(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            media: Vec::new(),
            is_error: true,
            details: None,
            display_preview: None,
            edit_diff: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolProgress {
    pub message: String,
}

pub type ProgressSink = mpsc::Sender<ToolProgress>;

/// YYC-263: typed-parameter helper for tool implementations. Wraps
/// `serde_json::from_value` so a tool can declare its params as a
/// `#[derive(Deserialize)]` struct and avoid the manual
/// `params["k"].as_str().unwrap_or("")` chain. The error path
/// returns a `ToolResult::err` with the deserialize message,
/// already shaped for the LLM.
///
/// Usage:
/// ```ignore
/// #[derive(serde::Deserialize)]
/// struct ReadFileParams {
///     path: String,
///     #[serde(default = "default_offset")]
///     offset: i64,
/// }
///
/// async fn call(&self, params: Value, _: CancellationToken, _progress: Option<crate::tools::ProgressSink>) -> Result<ToolResult> {
///     let p: ReadFileParams = match parse_tool_params(params) {
///         Ok(p) => p,
///         Err(e) => return Ok(e),
///     };
///     // use p.path, p.offset, ...
/// }
/// ```
///
/// The full migration (every tool gets a typed struct, the
/// `unwrap_or` chains die) is tracked under YYC-263.
pub fn parse_tool_params<T>(params: Value) -> std::result::Result<T, ToolResult>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(params).map_err(|e| {
        ToolResult::err(format!(
            "tool params failed to validate: {e}. Check the schema and retry."
        ))
    })
}

/// YYC-222: replay-safety classification for a tool.
///
/// `ReadOnly` — pure observation; safe to actually re-run during a
///   tool-replay pass (file reads, list dirs, search, embeddings
///   queries). The tool's output may differ from the recording if
///   the workspace has changed since the original run; replay
///   diffing surfaces that drift.
///
/// `Mutating` — touches local state (writes a file, edits a config,
///   spawns a child process whose effect outlives the call). Replay
///   refuses to re-run these live; the mock output is used.
///
/// `External` — reaches the network or any third-party system whose
///   side-effects can't be undone (web fetch, gateway dispatch). A
///   live re-run requires explicit user opt-in beyond the normal
///   replay confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaySafety {
    ReadOnly,
    Mutating,
    External,
}

impl ReplaySafety {
    pub fn as_str(self) -> &'static str {
        match self {
            ReplaySafety::ReadOnly => "read_only",
            ReplaySafety::Mutating => "mutating",
            ReplaySafety::External => "external",
        }
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
    async fn call(
        &self,
        params: Value,
        cancel: CancellationToken,
        progress: Option<ProgressSink>,
    ) -> Result<ToolResult>;

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

    /// YYC-222: classify how a replay engine should treat this tool.
    /// The default is the conservative one — if a tool's author
    /// hasn't thought about replay safety, the replay machinery
    /// should refuse to re-run it live and stick to mocked output.
    /// Read-only tools (file reads, code-graph queries) can be
    /// re-run safely against the recorded inputs; external tools
    /// (web fetch, network ops) need explicit user consent.
    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::Mutating
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// YYC-273: cap on the global diff sink. With concurrent tool dispatch
/// the previous `Option<EditDiff>` slot threw earlier writes away — the
/// last writer won, so a parallel pair of `edit_file` calls only ever
/// surfaced one diff in the TUI panel. The bounded queue keeps the
/// most-recent N edits so concurrent edits both surface and the TUI
/// can render a small history without unbounded memory growth.
const DIFF_SINK_CAP: usize = 8;

/// Shared edit history backed by a bounded queue. Producers push the
/// latest diff; consumers (TUI status panel, DiagnosticsHook fallback
/// path) ask for the most-recent entry — optionally filtered by tool
/// name so a stale unrelated entry can't trigger a hook.
#[derive(Debug)]
pub struct DiffSink {
    inner: parking_lot::Mutex<std::collections::VecDeque<EditDiff>>,
    cap: usize,
}

impl DiffSink {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: parking_lot::Mutex::new(std::collections::VecDeque::with_capacity(cap)),
            cap,
        }
    }

    pub fn push(&self, diff: EditDiff) {
        let mut q = self.inner.lock();
        q.push_back(diff);
        while q.len() > self.cap {
            q.pop_front();
        }
    }

    pub fn latest(&self) -> Option<EditDiff> {
        self.inner.lock().back().cloned()
    }

    /// Most-recent diff whose `tool` name matches. Used by
    /// `DiagnosticsHook` so a stale entry from a different tool can't
    /// re-trigger the wrong language server.
    pub fn latest_for_tool(&self, tool: &str) -> Option<EditDiff> {
        self.inner
            .lock()
            .iter()
            .rev()
            .find(|d| d.tool == tool)
            .cloned()
    }

    pub fn recent(&self) -> Vec<EditDiff> {
        self.inner.lock().iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }
}

pub type EditDiffSink = Arc<DiffSink>;

pub fn new_diff_sink() -> EditDiffSink {
    Arc::new(DiffSink::new(DIFF_SINK_CAP))
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

pub(crate) struct ToolRegistryAssembly<'a> {
    pub(crate) config: &'a crate::config::Config,
    pub(crate) active_provider: &'a crate::config::ProviderConfig,
    pub(crate) api_key: &'a str,
    pub(crate) cwd: PathBuf,
    pub(crate) diff_sink: EditDiffSink,
    pub(crate) lsp_manager: Arc<crate::code::lsp::LspManager>,
    pub(crate) pause_tx: Option<crate::pause::PauseSender>,
    pub(crate) pool: Option<&'a crate::runtime_pool::RuntimeResourcePool>,
    pub(crate) hooks: &'a mut crate::hooks::HookRegistry,
    pub(crate) memory: Arc<crate::memory::SessionStore>,
    pub(crate) session_id: &'a str,
    pub(crate) frontend_capabilities: Vec<crate::extensions::FrontendCapability>,
    pub(crate) tool_profile_override: Option<String>,
    pub(crate) orchestration: Arc<crate::orchestration::OrchestrationStore>,
    pub(crate) artifact_store: Arc<dyn crate::artifact::ArtifactStore>,
    pub(crate) current_run_id: Arc<parking_lot::Mutex<Option<crate::run_record::RunId>>>,
}

pub(crate) struct AssembledToolRegistry {
    pub(crate) registry: ToolRegistry,
    pub(crate) context: ToolContext,
    pub(crate) trust_profile: crate::trust::TrustProfile,
    pub(crate) daemon_extensions: usize,
    pub(crate) extension_tools: usize,
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

    pub(crate) fn assemble_for_agent(
        assembly: ToolRegistryAssembly<'_>,
    ) -> Result<AssembledToolRegistry> {
        let ToolRegistryAssembly {
            config,
            active_provider,
            api_key,
            cwd,
            diff_sink,
            lsp_manager,
            pause_tx,
            pool,
            hooks,
            memory,
            session_id,
            frontend_capabilities,
            tool_profile_override,
            orchestration,
            artifact_store,
            current_run_id,
        } = assembly;

        let context = ToolContext::probe(cwd.clone());
        let mut registry =
            Self::new_with_diff_and_lsp(Some(diff_sink.clone()), Some(lsp_manager), cwd.clone());

        if pause_tx.is_some() {
            registry.register(Arc::new(crate::tools::ask_user::AskUserTool::new(
                pause_tx.clone(),
            )));
            registry.register(Arc::new(crate::tools::file::PatchFile::with_pause(
                Some(diff_sink),
                pause_tx.clone(),
            )));
        }

        if config.embeddings.enabled {
            Self::register_embedding_tools(&mut registry, config, active_provider, api_key);
        }

        let mut daemon_extensions = 0;
        let mut extension_tools = 0;
        if let Some(pool) = pool {
            let ctx = crate::extensions::api::SessionExtensionCtx {
                cwd: cwd.clone(),
                session_id: session_id.to_string(),
                memory,
                frontend_capabilities,
                state: crate::extensions::ExtensionStateContext::new(
                    pool.extension_state_store(),
                    session_id.to_string(),
                    "__pending__",
                    Vec::new(),
                ),
            };
            let wired = pool
                .extension_registry()
                .wire_daemon_extensions_into_runtime(ctx, hooks, Some(&mut registry));
            daemon_extensions = wired.0;
            extension_tools = wired.1;
        }

        let spawn_tool = crate::tools::spawn::SpawnSubagentTool::with_store(
            Arc::new(config.clone()),
            orchestration,
        )
        .with_artifact_store(artifact_store)
        .with_parent_session_id(session_id.to_string())
        .with_parent_run_handle(current_run_id);
        registry.register(Arc::new(spawn_tool));

        let trust_profile = config.workspace_trust.resolve_for(&context.cwd);
        let resolved_profile_name = tool_profile_override
            .as_deref()
            .map(str::to_string)
            .or_else(|| config.tools.profile.clone())
            .or_else(|| {
                if trust_profile.reason.contains("matched") {
                    Some(trust_profile.capability_profile.clone())
                } else {
                    None
                }
            });
        if let Some(name) = &resolved_profile_name {
            let profile = config.tools.resolve_profile(name).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown tool capability profile `{name}`. Built-in: readonly, coding, \
                     reviewer, gateway-safe. User-defined go under [tools.profiles.<name>]."
                )
            })?;
            registry.apply_profile(&profile);
        }

        registry.filter_for_context(&context);

        Ok(AssembledToolRegistry {
            registry,
            context,
            trust_profile,
            daemon_extensions,
            extension_tools,
        })
    }

    fn register_embedding_tools(
        registry: &mut ToolRegistry,
        config: &crate::config::Config,
        active_provider: &crate::config::ProviderConfig,
        api_key: &str,
    ) {
        let parser_cache = Arc::new(crate::code::ParserCache::new());
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        match crate::code::embed::EmbeddingIndex::open(
            cwd,
            parser_cache,
            config.embeddings.clone(),
            active_provider.base_url.clone(),
            api_key.to_string().into(),
        ) {
            Ok(index) => {
                let arc = Arc::new(index);
                registry.register(Arc::new(
                    crate::tools::code_search::IndexEmbeddingsTool::new(arc.clone()),
                ));
                registry.register(Arc::new(
                    crate::tools::code_search::CodeSearchSemanticTool::new(arc),
                ));
            }
            Err(e) => tracing::warn!("embedding index unavailable: {e}"),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn register_extension_tools(
        &mut self,
        extension_id: &str,
        tools: Vec<Arc<dyn Tool>>,
    ) -> std::result::Result<usize, String> {
        let prefix = format!("{extension_id}_");
        let mut pending = Vec::new();
        for tool in tools {
            let name = format!("{prefix}{}", tool.name());
            if self.tools.contains_key(&name) || pending.iter().any(|(n, _)| n == &name) {
                return Err(format!("tool name collision for `{name}`"));
            }
            pending.push((name, tool));
        }
        let count = pending.len();
        for (name, tool) in pending {
            self.tools
                .insert(name.clone(), Arc::new(PrefixedTool { name, inner: tool }));
        }
        Ok(count)
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
        self.execute_with_progress(name, arguments, cancel, None)
            .await
    }

    pub async fn execute_with_progress(
        &self,
        name: &str,
        arguments: &str,
        cancel: CancellationToken,
        progress: Option<ProgressSink>,
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

        tool.call(params, cancel, progress).await
    }
}

struct PrefixedTool {
    name: String,
    inner: Arc<dyn Tool>,
}

#[async_trait::async_trait]
impl Tool for PrefixedTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn schema(&self) -> Value {
        self.inner.schema()
    }

    async fn call(
        &self,
        params: Value,
        cancel: CancellationToken,
        progress: Option<ProgressSink>,
    ) -> Result<ToolResult> {
        self.inner.call(params, cancel, progress).await
    }

    fn is_relevant(&self, ctx: &ToolContext) -> bool {
        self.inner.is_relevant(ctx)
    }

    fn dynamic_description(&self, ctx: &ToolContext) -> Option<String> {
        self.inner.dynamic_description(ctx)
    }

    fn replay_safety(&self) -> ReplaySafety {
        self.inner.replay_safety()
    }
}

pub fn details_from_tool_message(content: &str) -> Option<Value> {
    serde_json::from_str::<Value>(content)
        .ok()
        .and_then(|v| v.get("details").cloned())
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

    fn make_diff(tool: &str, path: &str) -> EditDiff {
        EditDiff {
            path: path.into(),
            tool: tool.into(),
            before: String::new(),
            after: String::new(),
            at: chrono::Local::now(),
        }
    }

    #[test]
    fn yyc263_parse_tool_params_returns_typed_struct_on_valid_input() {
        #[derive(serde::Deserialize, Debug, PartialEq, Eq)]
        struct Params {
            path: String,
            #[serde(default)]
            limit: i64,
        }
        let v = json!({"path": "src/main.rs", "limit": 42});
        let p: Params = parse_tool_params(v).unwrap();
        assert_eq!(p.path, "src/main.rs");
        assert_eq!(p.limit, 42);
    }

    #[test]
    fn yyc263_parse_tool_params_uses_serde_default_for_missing_field() {
        #[derive(serde::Deserialize, Debug)]
        struct Params {
            #[serde(default = "default_count")]
            count: i64,
        }
        fn default_count() -> i64 {
            7
        }
        let v = json!({});
        let p: Params = parse_tool_params(v).unwrap();
        assert_eq!(p.count, 7);
    }

    #[test]
    fn yyc263_parse_tool_params_returns_toolresult_err_on_bad_input() {
        #[derive(serde::Deserialize, Debug)]
        struct Params {
            #[allow(dead_code)]
            path: String,
        }
        let v = json!({"path": 42}); // wrong type
        let result = parse_tool_params::<Params>(v);
        let err = match result {
            Ok(_) => panic!("expected error"),
            Err(e) => e,
        };
        assert!(err.is_error);
        assert!(err.output.contains("failed to validate"));
    }

    #[test]
    fn diff_sink_push_then_latest_returns_last_pushed() {
        let s = DiffSink::new(4);
        assert!(s.latest().is_none());
        s.push(make_diff("write_file", "a.rs"));
        s.push(make_diff("edit_file", "b.rs"));
        let last = s.latest().unwrap();
        assert_eq!(last.tool, "edit_file");
        assert_eq!(last.path, "b.rs");
    }

    #[test]
    fn diff_sink_caps_at_capacity_dropping_oldest() {
        let s = DiffSink::new(3);
        for i in 0..5 {
            s.push(make_diff("edit_file", &format!("f{i}.rs")));
        }
        let recent = s.recent();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].path, "f2.rs");
        assert_eq!(recent[2].path, "f4.rs");
    }

    #[test]
    fn diff_sink_latest_for_tool_skips_unrelated_entries() {
        let s = DiffSink::new(4);
        s.push(make_diff("write_file", "wrote.rs"));
        s.push(make_diff("edit_file", "edited.rs"));
        // Concurrent dispatch: a parallel write happens after the edit.
        s.push(make_diff("write_file", "wrote_again.rs"));
        let edit_match = s.latest_for_tool("edit_file").unwrap();
        assert_eq!(edit_match.path, "edited.rs");
        let write_match = s.latest_for_tool("write_file").unwrap();
        assert_eq!(write_match.path, "wrote_again.rs");
        assert!(s.latest_for_tool("nonexistent").is_none());
    }

    #[test]
    fn diff_sink_concurrent_pushes_respect_cap_without_panic() {
        // Two parallel producers under heavy contention used to race
        // on the previous global last-writer slot. The bounded queue
        // must keep len ≤ cap regardless of interleaving — that's the
        // deterministic property worth pinning here. Whether both
        // tools' diffs end up in the final window depends on timing,
        // so the test doesn't assert that.
        let s = std::sync::Arc::new(DiffSink::new(8));
        let s1 = std::sync::Arc::clone(&s);
        let s2 = std::sync::Arc::clone(&s);
        let h1 = std::thread::spawn(move || {
            for i in 0..50 {
                s1.push(make_diff("edit_file", &format!("a{i}.rs")));
            }
        });
        let h2 = std::thread::spawn(move || {
            for i in 0..50 {
                s2.push(make_diff("write_file", &format!("b{i}.rs")));
            }
        });
        h1.join().unwrap();
        h2.join().unwrap();
        assert_eq!(s.len(), 8);
        assert_eq!(s.recent().len(), 8);
    }

    #[test]
    fn diff_sink_low_volume_concurrent_pushes_keep_both_producers() {
        // With push counts that fit inside the cap, both producers'
        // entries survive deterministically regardless of timing.
        let s = std::sync::Arc::new(DiffSink::new(8));
        let s1 = std::sync::Arc::clone(&s);
        let s2 = std::sync::Arc::clone(&s);
        let h1 = std::thread::spawn(move || {
            for i in 0..3 {
                s1.push(make_diff("edit_file", &format!("a{i}.rs")));
            }
        });
        let h2 = std::thread::spawn(move || {
            for i in 0..3 {
                s2.push(make_diff("write_file", &format!("b{i}.rs")));
            }
        });
        h1.join().unwrap();
        h2.join().unwrap();
        assert_eq!(s.len(), 6);
        assert!(s.latest_for_tool("edit_file").is_some());
        assert!(s.latest_for_tool("write_file").is_some());
    }

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
            .execute_with_progress("edit_file", &bogus_args, CancellationToken::new(), None)
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
            .execute_with_progress(
                "read_file",
                "{not valid json",
                CancellationToken::new(),
                None,
            )
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
            .execute_with_progress("write_file", "{}", CancellationToken::new(), None)
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
            .execute_with_progress("read_file", "[]", CancellationToken::new(), None)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("JSON object"), "got {msg:?}");
        assert!(msg.contains("Schema"), "got {msg:?}");
    }
}
