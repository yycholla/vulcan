//! LSP client (YYC-46).
//!
//! Speaks JSON-RPC 2.0 over stdio with Content-Length framing to per-
//! language servers (rust-analyzer, pyright, gopls, etc). Per-language
//! lifecycle is managed by `LspManager`: lazy spawn, kept alive for the
//! session, reaped on drop or `shutdown()`.
//!
//! Hand-rolled framing keeps the dep surface small (only `lsp-types`
//! for the structured request/response bodies). The async layer uses
//! tokio::process + tokio::sync::oneshot for request correlation.
//!
//! Notifications (`textDocument/publishDiagnostics`) feed a per-server
//! diagnostics cache so the upcoming `DiagnosticsHook` (YYC-51) can
//! query "what changed since the last edit?" without a roundtrip.

use crate::code::Language;
use anyhow::{Context, Result, anyhow};
use lsp_types::{
    ClientCapabilities, Diagnostic, DidOpenTextDocumentParams, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverParams, InitializeParams, InitializedParams, Location,
    Position, ReferenceContext, ReferenceParams, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, Uri, WorkspaceFolder,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, oneshot};

/// Default server commands per language. Overridable via config.
fn default_command(lang: Language) -> Option<(&'static str, &'static [&'static str])> {
    match lang {
        Language::Rust => Some(("rust-analyzer", &[])),
        Language::TypeScript | Language::JavaScript => {
            Some(("typescript-language-server", &["--stdio"]))
        }
        Language::Python => Some(("pyright-langserver", &["--stdio"])),
        Language::Go => Some(("gopls", &[])),
        Language::Json => None,
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct RpcMessage {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>;
type DiagCache = Arc<Mutex<HashMap<String, Vec<Diagnostic>>>>;

/// One LSP server subprocess + its JSON-RPC plumbing.
pub struct LspServer {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    next_id: Mutex<i64>,
    pending: Pending,
    diagnostics: DiagCache,
    workspace_root: PathBuf,
    lang: Language,
}

impl LspServer {
    /// Spawn the language server, send `initialize` + `initialized`,
    /// and start the background reader.
    pub async fn start(lang: Language, workspace_root: PathBuf) -> Result<Self> {
        let (cmd, args) = default_command(lang).ok_or_else(|| {
            anyhow!("No default LSP command for {}", lang.name())
        })?;
        let mut child = Command::new(cmd)
            .args(args)
            .current_dir(&workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to spawn LSP server '{cmd}' for {}. Is it installed?",
                    lang.name()
                )
            })?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let diagnostics: DiagCache = Arc::new(Mutex::new(HashMap::new()));

        // Reader task: pump messages off stdout, route by id (response)
        // or method (notification).
        let pending_clone = pending.clone();
        let diag_clone = diagnostics.clone();
        let lang_name = lang.name();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_message(&mut reader).await {
                    Ok(Some(msg)) => {
                        if let Some(Value::Number(n)) = msg.id {
                            if let Some(id) = n.as_i64() {
                                let mut p = pending_clone.lock().await;
                                if let Some(tx) = p.remove(&id) {
                                    let r = if let Some(err) = msg.error {
                                        Err(err.to_string())
                                    } else {
                                        Ok(msg.result.unwrap_or(Value::Null))
                                    };
                                    let _ = tx.send(r);
                                }
                            }
                        } else if let Some(method) = msg.method.as_deref() {
                            if method == "textDocument/publishDiagnostics" {
                                if let Some(params) = msg.params {
                                    if let (Some(uri), Some(diags)) = (
                                        params.get("uri").and_then(|v| v.as_str()),
                                        params.get("diagnostics"),
                                    ) {
                                        if let Ok(parsed) = serde_json::from_value::<
                                            Vec<Diagnostic>,
                                        >(
                                            diags.clone()
                                        ) {
                                            diag_clone
                                                .lock()
                                                .await
                                                .insert(uri.to_string(), parsed);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        tracing::warn!("LSP server ({lang_name}) stdout closed");
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("LSP read error ({lang_name}): {e}");
                        break;
                    }
                }
            }
        });

        let server = Self {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            next_id: Mutex::new(1),
            pending,
            diagnostics,
            workspace_root: workspace_root.clone(),
            lang,
        };

        // Initialize handshake.
        let workspace_uri = path_to_uri(&workspace_root)?;
        #[allow(deprecated)]
        let init = InitializeParams {
            process_id: Some(std::process::id()),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: workspace_uri.clone(),
                name: workspace_root
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            }]),
            capabilities: ClientCapabilities::default(),
            ..Default::default()
        };
        server
            .request::<lsp_types::InitializeResult, _>("initialize", init)
            .await?;
        server
            .notify("initialized", InitializedParams {})
            .await?;
        Ok(server)
    }

    async fn next_id(&self) -> i64 {
        let mut id = self.next_id.lock().await;
        let n = *id;
        *id += 1;
        n
    }

    /// Send a request and await the response. Returns parsed `R`.
    pub async fn request<R, P>(&self, method: &str, params: P) -> Result<R>
    where
        R: for<'de> Deserialize<'de>,
        P: Serialize,
    {
        let id = self.next_id().await;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": serde_json::to_value(&params)?,
        });
        write_message(&self.stdin, &msg).await?;

        let raw = tokio::time::timeout(Duration::from_secs(30), rx)
            .await
            .map_err(|_| anyhow!("LSP request '{method}' timed out"))?
            .map_err(|_| anyhow!("LSP response channel closed for '{method}'"))?;
        let value = raw.map_err(|e| anyhow!("LSP error on '{method}': {e}"))?;
        Ok(serde_json::from_value(value).context("decode LSP response")?)
    }

    /// Send a notification (no response).
    pub async fn notify<P: Serialize>(&self, method: &str, params: P) -> Result<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": serde_json::to_value(&params)?,
        });
        write_message(&self.stdin, &msg).await
    }

    /// Open a file (sends `textDocument/didOpen`). Idempotent — safe to
    /// call before each tool invocation; the server tracks state.
    pub async fn did_open(&self, path: &Path, source: &str) -> Result<()> {
        let uri = path_to_uri(path)?;
        let lang_id = self.lang.name().to_string();
        let item = TextDocumentItem {
            uri,
            language_id: lang_id,
            version: 1,
            text: source.to_string(),
        };
        self.notify(
            "textDocument/didOpen",
            DidOpenTextDocumentParams {
                text_document: item,
            },
        )
        .await
    }

    /// Snapshot the cached diagnostics for `path` (populated by the
    /// reader from `publishDiagnostics` notifications).
    pub async fn cached_diagnostics(&self, path: &Path) -> Vec<Diagnostic> {
        let uri = match path_to_uri(path) {
            Ok(u) => u.to_string(),
            Err(_) => return Vec::new(),
        };
        self.diagnostics
            .lock()
            .await
            .get(&uri)
            .cloned()
            .unwrap_or_default()
    }

    /// Workspace root the server was started against — useful for path
    /// validation in tools.
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn language(&self) -> Language {
        self.lang
    }

    /// Politely shut the server down. Best-effort; on timeout the
    /// child will be killed via `kill_on_drop`.
    pub async fn shutdown(&self) {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            self.request::<Value, _>("shutdown", json!(null)),
        )
        .await;
        let _ = self.notify("exit", json!(null)).await;
        let _ = self.child.lock().await.kill().await;
    }
}

async fn read_message<R>(reader: &mut BufReader<R>) -> Result<Option<RpcMessage>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(|c| c == '\r' || c == '\n');
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(rest.trim().parse()?);
        }
    }
    let len = content_length.ok_or_else(|| anyhow!("missing Content-Length"))?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    let msg: RpcMessage = serde_json::from_slice(&buf)?;
    Ok(Some(msg))
}

async fn write_message(stdin: &Mutex<ChildStdin>, msg: &Value) -> Result<()> {
    let body = serde_json::to_vec(msg)?;
    let mut guard = stdin.lock().await;
    guard
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await?;
    guard.write_all(&body).await?;
    guard.flush().await?;
    Ok(())
}

fn path_to_uri(path: &Path) -> Result<Uri> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let s = format!("file://{}", abs.to_string_lossy());
    Uri::from_str(&s).map_err(|e| anyhow!("invalid URI '{s}': {e}"))
}

/// Per-language LSP server pool. Spawns lazily on first request for a
/// given language; reuses the live instance on subsequent calls.
pub struct LspManager {
    workspace_root: PathBuf,
    servers: Mutex<HashMap<Language, Arc<LspServer>>>,
}

impl LspManager {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            servers: Mutex::new(HashMap::new()),
        }
    }

    /// Get or spawn the server for `lang`. Returns Err if the language
    /// has no default server command or the binary isn't on PATH.
    pub async fn server(&self, lang: Language) -> Result<Arc<LspServer>> {
        {
            let servers = self.servers.lock().await;
            if let Some(s) = servers.get(&lang) {
                return Ok(s.clone());
            }
        }
        let server = Arc::new(LspServer::start(lang, self.workspace_root.clone()).await?);
        let mut servers = self.servers.lock().await;
        // Race-protect: another caller may have inserted while we spawned.
        Ok(servers.entry(lang).or_insert(server).clone())
    }

    /// Reap all running servers. Called from `BeforeAgentEnd` so the
    /// children don't outlive the agent.
    pub async fn shutdown_all(&self) {
        let mut servers = self.servers.lock().await;
        for (_, s) in servers.drain() {
            s.shutdown().await;
        }
    }
}

// ─── high-level helpers used by the LSP tools ──────────────────────────

pub async fn goto_definition(
    server: &LspServer,
    path: &Path,
    line: u32,
    character: u32,
) -> Result<Option<Vec<Location>>> {
    let source = tokio::fs::read_to_string(path).await?;
    server.did_open(path, &source).await?;
    let uri = path_to_uri(path)?;
    let params = GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position { line, character },
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let resp: Option<GotoDefinitionResponse> = server
        .request("textDocument/definition", params)
        .await
        .ok();
    Ok(resp.map(|r| match r {
        GotoDefinitionResponse::Scalar(loc) => vec![loc],
        GotoDefinitionResponse::Array(v) => v,
        GotoDefinitionResponse::Link(links) => links
            .into_iter()
            .map(|l| Location {
                uri: l.target_uri,
                range: l.target_range,
            })
            .collect(),
    }))
}

pub async fn find_references(
    server: &LspServer,
    path: &Path,
    line: u32,
    character: u32,
) -> Result<Option<Vec<Location>>> {
    let source = tokio::fs::read_to_string(path).await?;
    server.did_open(path, &source).await?;
    let uri = path_to_uri(path)?;
    let params = ReferenceParams {
        text_document_position: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position { line, character },
        },
        context: ReferenceContext {
            include_declaration: true,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let resp: Option<Vec<Location>> = server
        .request("textDocument/references", params)
        .await
        .ok();
    Ok(resp)
}

pub async fn hover(
    server: &LspServer,
    path: &Path,
    line: u32,
    character: u32,
) -> Result<Option<Hover>> {
    let source = tokio::fs::read_to_string(path).await?;
    server.did_open(path, &source).await?;
    let uri = path_to_uri(path)?;
    let params = HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position { line, character },
        },
        work_done_progress_params: Default::default(),
    };
    let resp: Option<Hover> = server.request("textDocument/hover", params).await.ok();
    Ok(resp)
}

/// Open the file then return what the server has cached. Diagnostics
/// arrive asynchronously after `didOpen` so we wait briefly for the
/// first publish; later calls (after edits) get the latest snapshot
/// without delay.
pub async fn diagnostics_for(
    server: &LspServer,
    path: &Path,
) -> Result<Vec<Diagnostic>> {
    let source = tokio::fs::read_to_string(path).await?;
    server.did_open(path, &source).await?;
    // Give the server up to 1.5s to publish initial diagnostics on
    // first open. Subsequent calls return immediately.
    let initial = server.cached_diagnostics(path).await;
    if !initial.is_empty() {
        return Ok(initial);
    }
    for _ in 0..15 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let cached = server.cached_diagnostics(path).await;
        if !cached.is_empty() {
            return Ok(cached);
        }
    }
    Ok(server.cached_diagnostics(path).await)
}
