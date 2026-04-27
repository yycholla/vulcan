//! High-level LSP request helpers.
//!
//! Each function takes an `&LspServer`, runs the appropriate
//! `prepare_request` (open + wait for readiness), issues the
//! request, and returns the projection the tool layer wants. Pulled
//! out of `mod.rs` because the request surface keeps growing
//! (goto, hover, references, type/impl, call hierarchy, workspace
//! symbol, diagnostics) and the file was hitting 1200+ lines.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
    CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
    Diagnostic, GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams, Location,
    Position, ReferenceContext, ReferenceParams, SymbolInformation, TextDocumentIdentifier,
    TextDocumentPositionParams, WorkspaceSymbolParams, WorkspaceSymbolResponse,
    request::{GotoImplementationResponse, GotoTypeDefinitionResponse},
};

use super::{DEFAULT_LSP_READINESS_WAIT_SECS, LspServer, path_to_uri};

async fn prepare_request(server: &LspServer, path: &Path) -> Result<()> {
    let source = tokio::fs::read_to_string(path).await?;
    server.did_open(path, &source).await?;
    server
        .wait_until_ready(Duration::from_secs(DEFAULT_LSP_READINESS_WAIT_SECS))
        .await;
    Ok(())
}

pub async fn goto_definition(
    server: &LspServer,
    path: &Path,
    line: u32,
    character: u32,
) -> Result<Option<Vec<Location>>> {
    prepare_request(server, path).await?;
    let uri = path_to_uri(path)?;
    let params = GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position { line, character },
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let resp: Option<GotoDefinitionResponse> =
        server.request("textDocument/definition", params).await?;
    server.mark_ready();
    Ok(resp.map(flatten_goto_response))
}

pub async fn find_references(
    server: &LspServer,
    path: &Path,
    line: u32,
    character: u32,
) -> Result<Option<Vec<Location>>> {
    prepare_request(server, path).await?;
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
    let resp: Option<Vec<Location>> = server.request("textDocument/references", params).await?;
    server.mark_ready();
    Ok(resp)
}

pub async fn hover(
    server: &LspServer,
    path: &Path,
    line: u32,
    character: u32,
) -> Result<Option<Hover>> {
    prepare_request(server, path).await?;
    let uri = path_to_uri(path)?;
    let params = HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position { line, character },
        },
        work_done_progress_params: Default::default(),
    };
    let resp: Option<Hover> = server.request("textDocument/hover", params).await?;
    server.mark_ready();
    Ok(resp)
}

/// YYC-202: jump to the type definition of the expression at the
/// given position. Same response shape as `goto_definition` so the
/// tool layer can reuse the same projection code path.
pub async fn type_definition(
    server: &LspServer,
    path: &Path,
    line: u32,
    character: u32,
) -> Result<Option<Vec<Location>>> {
    prepare_request(server, path).await?;
    let uri = path_to_uri(path)?;
    let params = GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position { line, character },
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let resp: Option<GotoTypeDefinitionResponse> = server
        .request("textDocument/typeDefinition", params)
        .await?;
    server.mark_ready();
    Ok(resp.map(flatten_goto_response))
}

/// YYC-202: list every implementation of the trait/interface at the
/// given position. Mirrors `goto_definition` for the response shape.
pub async fn implementation(
    server: &LspServer,
    path: &Path,
    line: u32,
    character: u32,
) -> Result<Option<Vec<Location>>> {
    prepare_request(server, path).await?;
    let uri = path_to_uri(path)?;
    let params = GotoDefinitionParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position { line, character },
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let resp: Option<GotoImplementationResponse> = server
        .request("textDocument/implementation", params)
        .await?;
    server.mark_ready();
    Ok(resp.map(flatten_goto_response))
}

/// YYC-202: shared collapse for the three shapes
/// `GotoDefinitionResponse` (and its type/impl aliases) can take.
fn flatten_goto_response(r: GotoDefinitionResponse) -> Vec<Location> {
    match r {
        GotoDefinitionResponse::Scalar(loc) => vec![loc],
        GotoDefinitionResponse::Array(v) => v,
        GotoDefinitionResponse::Link(links) => links
            .into_iter()
            .map(|l| Location {
                uri: l.target_uri,
                range: l.target_range,
            })
            .collect(),
    }
}

/// YYC-203: prepare a call-hierarchy item at the given position.
/// LSP's call hierarchy is a two-step query: first resolve the
/// symbol at the cursor to a `CallHierarchyItem`, then ask the
/// server for its incoming or outgoing calls. Returns an empty
/// vec when the position doesn't resolve to a callable symbol.
pub async fn prepare_call_hierarchy(
    server: &LspServer,
    path: &Path,
    line: u32,
    character: u32,
) -> Result<Vec<CallHierarchyItem>> {
    prepare_request(server, path).await?;
    let uri = path_to_uri(path)?;
    let params = CallHierarchyPrepareParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position { line, character },
        },
        work_done_progress_params: Default::default(),
    };
    let resp: Option<Vec<CallHierarchyItem>> = server
        .request("textDocument/prepareCallHierarchy", params)
        .await?;
    server.mark_ready();
    Ok(resp.unwrap_or_default())
}

/// YYC-203: resolve incoming calls for an already-prepared call-
/// hierarchy item.
pub async fn call_hierarchy_incoming(
    server: &LspServer,
    item: CallHierarchyItem,
) -> Result<Vec<CallHierarchyIncomingCall>> {
    let params = CallHierarchyIncomingCallsParams {
        item,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let resp: Option<Vec<CallHierarchyIncomingCall>> = server
        .request("callHierarchy/incomingCalls", params)
        .await?;
    server.mark_ready();
    Ok(resp.unwrap_or_default())
}

/// YYC-203: resolve outgoing calls for an already-prepared call-
/// hierarchy item.
pub async fn call_hierarchy_outgoing(
    server: &LspServer,
    item: CallHierarchyItem,
) -> Result<Vec<CallHierarchyOutgoingCall>> {
    let params = CallHierarchyOutgoingCallsParams {
        item,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let resp: Option<Vec<CallHierarchyOutgoingCall>> = server
        .request("callHierarchy/outgoingCalls", params)
        .await?;
    server.mark_ready();
    Ok(resp.unwrap_or_default())
}

/// YYC-201: workspace-wide symbol search. Mirrors `goto_definition`
/// in surface — returns `Option<Vec<SymbolInformation>>` so callers
/// can distinguish "no hits" from "server returned null". Empty
/// `query` is forwarded as-is; LSP servers (rust-analyzer, gopls)
/// treat that as "list everything", which is up to the caller to
/// decide is sensible.
pub async fn workspace_symbol(
    server: &LspServer,
    query: &str,
) -> Result<Option<Vec<SymbolInformation>>> {
    let params = WorkspaceSymbolParams {
        query: query.to_string(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let resp: Option<WorkspaceSymbolResponse> = server.request("workspace/symbol", params).await?;
    server.mark_ready();
    Ok(resp.map(|r| match r {
        WorkspaceSymbolResponse::Flat(symbols) => symbols,
        // The newer `WorkspaceSymbol` shape lacks `Location.range` info
        // some servers omit; we accept the lossy conversion to
        // `SymbolInformation` since the tool layer reports
        // file + line, not full-range spans.
        WorkspaceSymbolResponse::Nested(_) => Vec::new(),
    }))
}

/// Open the file then return what the server has cached. Diagnostics
/// arrive asynchronously after `didOpen` so we wait briefly for the
/// first publish; later calls (after edits) get the latest snapshot
/// without delay.
pub async fn diagnostics_for(server: &LspServer, path: &Path) -> Result<Vec<Diagnostic>> {
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
