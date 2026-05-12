//! AST-aware structural edit tools (YYC-49).
//!
//! Replaces fuzzy-string `edit_file` for the cases where structural
//! anchors are stronger:
//!
//! - `replace_function_body`: locate a function/method by name via
//!   tree-sitter, splice in a new body. Idempotent — re-running with a
//!   renamed symbol fails loudly rather than corrupting unrelated code.
//! - `rename_symbol`: defers to LSP `textDocument/rename` for
//!   workspace-correct renames; surfaces the proposed edits without
//!   applying them in v1 (caller agent can read them and decide).
//!
//! - `add_method`: add a method to a Rust impl block or
//!   Python/TypeScript/JavaScript class using tree-sitter anchors.
//! - `add_import`: add deterministic, idempotent imports for Rust,
//!   Python, TypeScript, and JavaScript.
//!
//! Unsupported language/tool combinations fail clearly rather than
//! silently falling back to fuzzy text replacement.

use crate::code::Language;
use crate::code::lsp::LspManager;
use crate::tools::{EditDiff, Tool, ToolResult, parse_tool_params};
use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use lsp_types::{
    AnnotatedTextEdit, DocumentChangeOperation, DocumentChanges, OneOf, Position, TextDocumentEdit,
    TextEdit, Uri, WorkspaceEdit,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tree_sitter::{Parser as TsParser, Query, QueryCursor, StreamingIterator};

#[derive(Deserialize)]
struct ReplaceFunctionBodyParams {
    path: String,
    symbol: String,
    new_body: String,
}

#[derive(Deserialize)]
struct RenameSymbolParams {
    path: String,
    line: u64,
    #[serde(default)]
    character: u64,
    new_name: String,
}

#[derive(Deserialize)]
struct AddMethodParams {
    path: String,
    class_or_struct: String,
    method_source: String,
}

#[derive(Deserialize)]
struct AddImportParams {
    path: String,
    import_statement: String,
}

fn grammar_for(lang: Language) -> Option<tree_sitter::Language> {
    match lang {
        Language::Rust => Some(tree_sitter_rust::LANGUAGE.into()),
        Language::Python => Some(tree_sitter_python::LANGUAGE.into()),
        Language::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        Language::JavaScript => Some(tree_sitter_javascript::LANGUAGE.into()),
        Language::Go => Some(tree_sitter_go::LANGUAGE.into()),
        Language::Json => None,
    }
}

fn parse_source(
    lang: Language,
    source: &str,
) -> Result<(tree_sitter::Language, tree_sitter::Tree)> {
    let grammar = grammar_for(lang).ok_or_else(|| {
        anyhow::anyhow!("Unsupported language for structural edits: {}", lang.name())
    })?;
    let mut parser = TsParser::new();
    parser
        .set_language(&grammar)
        .map_err(|e| anyhow::anyhow!("set_language: {e}"))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("parse failed"))?;
    Ok((grammar, tree))
}

fn ensure_parseable(lang: Language, source: &str, operation: &str) -> Result<()> {
    let (_, tree) = parse_source(lang, source)?;
    if tree.root_node().has_error() {
        bail!(
            "{operation} would leave {} with tree-sitter parse errors",
            lang.name()
        );
    }
    Ok(())
}

fn line_indent_at(source: &str, byte: usize) -> String {
    let line_start = source[..byte].rfind('\n').map(|i| i + 1).unwrap_or(0);
    source[line_start..byte]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

fn indent_block(block: &str, indent: &str) -> String {
    let trimmed = block.trim_matches('\n');
    let mut out = String::new();
    for (i, line) in trimmed.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if line.trim().is_empty() {
            out.push_str(line);
        } else {
            out.push_str(indent);
            out.push_str(line);
        }
    }
    out
}

fn method_name_from_source(lang: Language, method_source: &str) -> Option<String> {
    let text = method_source.trim_start();
    match lang {
        Language::Rust => text
            .split_once("fn ")
            .and_then(|(_, rest)| {
                rest.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .next()
            })
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        Language::Python => text
            .strip_prefix("def ")
            .and_then(|rest| {
                rest.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .next()
            })
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        Language::TypeScript | Language::JavaScript => text
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
            .find(|s| {
                !s.is_empty()
                    && *s != "async"
                    && *s != "public"
                    && *s != "private"
                    && *s != "protected"
                    && *s != "static"
            })
            .map(str::to_string),
        Language::Go | Language::Json => None,
    }
}

fn find_add_method_anchor(
    lang: Language,
    source: &str,
    class_or_struct: &str,
) -> Result<Option<(usize, usize, String)>> {
    let (grammar, tree) = parse_source(lang, source)?;
    let query_text = match lang {
        Language::Rust => {
            "(impl_item type: (type_identifier) @name body: (declaration_list) @body)"
        }
        Language::Python => "(class_definition name: (identifier) @name body: (block) @body)",
        Language::TypeScript => {
            "(class_declaration name: (type_identifier) @name body: (class_body) @body)"
        }
        Language::JavaScript => {
            "(class_declaration name: (identifier) @name body: (class_body) @body)"
        }
        Language::Go => return Ok(None),
        Language::Json => return Ok(None),
    };
    let query = Query::new(&grammar, query_text).map_err(|e| anyhow::anyhow!("query: {e}"))?;
    let mut cursor = QueryCursor::new();
    let mut iter = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let name_idx = query.capture_index_for_name("name");
    let body_idx = query.capture_index_for_name("body");
    while let Some(m) = iter.next() {
        let mut name_text: Option<&str> = None;
        let mut body: Option<tree_sitter::Node<'_>> = None;
        for cap in m.captures {
            if Some(cap.index) == name_idx {
                name_text = cap.node.utf8_text(source.as_bytes()).ok();
            } else if Some(cap.index) == body_idx {
                body = Some(cap.node);
            }
        }
        if let (Some(n), Some(body)) = (name_text, body)
            && n == class_or_struct
        {
            let close_brace = match lang {
                Language::Rust | Language::TypeScript | Language::JavaScript => {
                    body.end_byte().saturating_sub(1)
                }
                Language::Python => body.end_byte(),
                Language::Go | Language::Json => unreachable!(),
            };
            let member_indent = match lang {
                Language::Rust | Language::TypeScript | Language::JavaScript => {
                    let base_indent = line_indent_at(source, close_brace);
                    format!("{base_indent}    ")
                }
                // Python class bodies are indentation-delimited; the body
                // node starts at the first member, so its line indent is
                // already the correct indentation for a new method.
                Language::Python => line_indent_at(source, body.start_byte()),
                Language::Go | Language::Json => unreachable!(),
            };
            return Ok(Some((body.start_byte(), close_brace, member_indent)));
        }
    }
    Ok(None)
}

fn add_method_to_source(
    lang: Language,
    source: &str,
    class_or_struct: &str,
    method_source: &str,
) -> Result<StructuralEdit> {
    if matches!(lang, Language::Go | Language::Json) {
        return Ok(StructuralEdit::Unsupported(format!(
            "add_method is not supported for {}; supported languages: rust, python, typescript, javascript",
            lang.name()
        )));
    }
    let Some((body_start, insert_at, member_indent)) =
        find_add_method_anchor(lang, source, class_or_struct)?
    else {
        let target = if lang == Language::Rust {
            "impl block"
        } else {
            "class"
        };
        return Ok(StructuralEdit::Error(format!(
            "Could not find {target} for `{class_or_struct}` in file. Use `code_outline` to inspect available symbols."
        )));
    };
    if let Some(method_name) = method_name_from_source(lang, method_source) {
        let body = &source[body_start..insert_at];
        let needle = match lang {
            Language::Rust => format!("fn {method_name}"),
            Language::Python => format!("def {method_name}"),
            Language::TypeScript | Language::JavaScript => method_name.clone(),
            Language::Go | Language::Json => unreachable!(),
        };
        if body.contains(&needle) {
            return Ok(StructuralEdit::Unchanged(format!(
                "Method `{}` already exists on `{}`",
                method_name, class_or_struct
            )));
        }
    }
    let method = indent_block(method_source, &member_indent);
    let mut insertion = String::new();
    if !source[..insert_at].ends_with('\n') {
        insertion.push('\n');
    }
    insertion.push('\n');
    insertion.push_str(&method);
    insertion.push('\n');
    let mut new_source = String::with_capacity(source.len() + insertion.len());
    new_source.push_str(&source[..insert_at]);
    new_source.push_str(&insertion);
    new_source.push_str(&source[insert_at..]);
    ensure_parseable(lang, &new_source, "add_method")?;
    Ok(StructuralEdit::Changed(new_source))
}

#[derive(Debug)]
enum StructuralEdit {
    Changed(String),
    Unchanged(String),
    Unsupported(String),
    Error(String),
}

fn canonical_import(lang: Language, import_statement: &str) -> Result<String, String> {
    let trimmed = import_statement.trim();
    if trimmed.is_empty() {
        return Err("import_statement must not be empty".into());
    }
    match lang {
        Language::Rust => {
            if !trimmed.starts_with("use ") {
                return Err("Rust imports must start with `use `".into());
            }
            Ok(if trimmed.ends_with(';') {
                trimmed.into()
            } else {
                format!("{trimmed};")
            })
        }
        Language::Python => {
            if !(trimmed.starts_with("import ") || trimmed.starts_with("from ")) {
                return Err("Python imports must start with `import ` or `from `".into());
            }
            Ok(trimmed.into())
        }
        Language::TypeScript | Language::JavaScript => {
            if !trimmed.starts_with("import ") {
                return Err(format!("{} imports must start with `import `", lang.name()));
            }
            Ok(if trimmed.ends_with(';') {
                trimmed.into()
            } else {
                format!("{trimmed};")
            })
        }
        Language::Go | Language::Json => Err(format!(
            "add_import is not supported for {}; supported languages: rust, python, typescript, javascript",
            lang.name()
        )),
    }
}

fn split_lines(source: &str) -> Vec<String> {
    if source.is_empty() {
        return Vec::new();
    }
    source.split_inclusive('\n').map(str::to_string).collect()
}

fn line_trimmed(line: &str) -> &str {
    line.trim_end_matches('\n').trim_end_matches('\r').trim()
}

fn is_import_line(lang: Language, trimmed: &str) -> bool {
    match lang {
        Language::Rust => trimmed.starts_with("use "),
        Language::Python => trimmed.starts_with("import ") || trimmed.starts_with("from "),
        Language::TypeScript | Language::JavaScript => trimmed.starts_with("import "),
        Language::Go | Language::Json => false,
    }
}

fn python_import_start(lines: &[String]) -> usize {
    let mut i = 0;
    if lines.get(i).is_some_and(|l| l.starts_with("#!")) {
        i += 1;
    }
    if lines
        .get(i)
        .is_some_and(|l| l.contains("coding") || l.contains("encoding"))
    {
        i += 1;
    }
    while lines.get(i).is_some_and(|l| line_trimmed(l).is_empty()) {
        i += 1;
    }
    if let Some(line) = lines.get(i) {
        let trimmed = line_trimmed(line);
        if trimmed.starts_with("\"\"\"") || trimmed.starts_with("'''") {
            let quote = if trimmed.starts_with("\"\"\"") {
                "\"\"\""
            } else {
                "'''"
            };
            i += 1;
            if !trimmed[3..].contains(quote) {
                while i < lines.len() {
                    if lines[i].contains(quote) {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            while lines.get(i).is_some_and(|l| line_trimmed(l).is_empty()) {
                i += 1;
            }
        }
    }
    i
}

fn import_start(lang: Language, lines: &[String]) -> usize {
    match lang {
        Language::Rust => {
            let mut i = 0;
            while i < lines.len() {
                let t = line_trimmed(&lines[i]);
                if t.is_empty()
                    || t.starts_with("//!")
                    || t.starts_with("/*!")
                    || t.starts_with("#![")
                {
                    i += 1;
                } else {
                    break;
                }
            }
            i
        }
        Language::Python => python_import_start(lines),
        Language::TypeScript | Language::JavaScript => {
            if lines.first().is_some_and(|l| l.starts_with("#!")) {
                1
            } else {
                0
            }
        }
        Language::Go | Language::Json => 0,
    }
}

fn add_import_to_source(lang: Language, source: &str, import_statement: &str) -> StructuralEdit {
    let import = match canonical_import(lang, import_statement) {
        Ok(import) => import,
        Err(e) if e.starts_with("add_import is not supported") => {
            return StructuralEdit::Unsupported(e);
        }
        Err(e) => return StructuralEdit::Error(e),
    };
    let mut lines = split_lines(source);
    if lines.iter().any(|l| line_trimmed(l) == import) {
        return StructuralEdit::Unchanged(format!("Import `{import}` already exists"));
    }
    let start = import_start(lang, &lines);
    let mut end = start;
    while end < lines.len() && is_import_line(lang, line_trimmed(&lines[end])) {
        end += 1;
    }
    let mut imports: Vec<String> = lines[start..end]
        .iter()
        .map(|l| line_trimmed(l).to_string())
        .collect();
    imports.push(import);
    imports.sort();
    imports.dedup();
    let replacement: Vec<String> = imports.into_iter().map(|i| format!("{i}\n")).collect();
    if start == end {
        lines.splice(
            start..end,
            replacement
                .into_iter()
                .chain(std::iter::once("\n".to_string())),
        );
    } else {
        lines.splice(start..end, replacement);
    }
    let new_source = lines.concat();
    if let Err(e) = ensure_parseable(lang, &new_source, "add_import") {
        return StructuralEdit::Error(e.to_string());
    }
    StructuralEdit::Changed(new_source)
}

fn edit_result(tool: &str, path: &str, before: &str, after: &str, output: String) -> ToolResult {
    let diff = EditDiff {
        path: path.to_string(),
        tool: tool.into(),
        before: crate::tools::snippet(before, 6, 800),
        after: crate::tools::snippet(after, 6, 800),
        at: chrono::Local::now(),
    };
    ToolResult::ok(output).with_edit_diff(diff)
}

/// Find the body node of a function/method named `symbol` in `source`.
/// Returns `(body_start_byte, body_end_byte)` so the caller can splice.
/// Tree-sitter's `body:` field gives us the brace-delimited block that
/// includes the leading `{` and trailing `}` — replacing that node
/// preserves the signature line untouched.
fn find_function_body_range(
    lang: Language,
    source: &str,
    symbol: &str,
) -> Result<Option<(usize, usize)>> {
    let mut parser = TsParser::new();
    let grammar: tree_sitter::Language = match lang {
        Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::Go => tree_sitter_go::LANGUAGE.into(),
        Language::Json => return Ok(None),
    };
    parser
        .set_language(&grammar)
        .map_err(|e| anyhow::anyhow!("set_language: {e}"))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("parse failed"))?;

    // Per-language query: capture the function name + its body block.
    let query_text = match lang {
        Language::Rust => "(function_item name: (identifier) @name body: (block) @body)",
        Language::Python => "(function_definition name: (identifier) @name body: (block) @body)",
        Language::TypeScript | Language::JavaScript => {
            "(function_declaration name: (identifier) @name body: (statement_block) @body)"
        }
        Language::Go => "(function_declaration name: (identifier) @name body: (block) @body)",
        Language::Json => return Ok(None),
    };
    let query = Query::new(&grammar, query_text).map_err(|e| anyhow::anyhow!("query: {e}"))?;
    let mut cursor = QueryCursor::new();
    let mut iter = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let name_idx = query.capture_index_for_name("name");
    let body_idx = query.capture_index_for_name("body");
    while let Some(m) = iter.next() {
        let mut name_text: Option<&str> = None;
        let mut body_range: Option<(usize, usize)> = None;
        for cap in m.captures {
            if Some(cap.index) == name_idx {
                name_text = cap.node.utf8_text(source.as_bytes()).ok();
            } else if Some(cap.index) == body_idx {
                body_range = Some((cap.node.start_byte(), cap.node.end_byte()));
            }
        }
        if let (Some(n), Some(range)) = (name_text, body_range)
            && n == symbol
        {
            return Ok(Some(range));
        }
    }
    Ok(None)
}

#[derive(Clone)]
pub struct ReplaceFunctionBodyTool;

#[async_trait]
impl Tool for ReplaceFunctionBodyTool {
    fn name(&self) -> &str {
        "replace_function_body"
    }
    fn description(&self) -> &str {
        "Replace just the body of a named function/method (the brace-delimited block) — idempotent and structural; fails loudly when the symbol is missing rather than corrupting unrelated code. `new_body` should include the surrounding `{ ... }` braces."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Source file" },
                "symbol": { "type": "string", "description": "Function/method name (case-sensitive). First match wins." },
                "new_body": {
                    "type": "string",
                    "description": "Full new body INCLUDING the outer braces, e.g. `{\\n    let x = 42;\\n    x + 1\\n}`"
                }
            },
            "required": ["path", "symbol", "new_body"]
        })
    }
    async fn call(
        &self,
        params: Value,
        _cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: ReplaceFunctionBodyParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let path = p.path.as_str();
        let symbol = p.symbol.as_str();
        let new_body = p.new_body.as_str();
        let pb = PathBuf::from(path);
        let lang = match Language::from_path(&pb) {
            Some(l) => l,
            None => {
                return Ok(ToolResult::err(format!(
                    "Unsupported file type for replace_function_body: {path}"
                )));
            }
        };
        let source = tokio::fs::read_to_string(path).await?;
        let range = match find_function_body_range(lang, &source, symbol)? {
            Some(r) => r,
            None => {
                return Ok(ToolResult::err(format!(
                    "Function '{symbol}' not found in {path}. Use `code_outline` to see available symbols."
                )));
            }
        };

        let mut new_source = String::with_capacity(source.len() + new_body.len());
        new_source.push_str(&source[..range.0]);
        new_source.push_str(new_body);
        new_source.push_str(&source[range.1..]);
        tokio::fs::write(path, &new_source).await?;
        Ok(ToolResult::ok(format!(
            "Replaced body of `{symbol}` in {path} ({} bytes → {} bytes)",
            range.1 - range.0,
            new_body.len()
        )))
    }
}

#[derive(Clone)]
pub struct AddMethodTool;

#[async_trait]
impl Tool for AddMethodTool {
    fn name(&self) -> &str {
        "add_method"
    }

    fn description(&self) -> &str {
        "Add a method to a Rust impl block or Python/TypeScript/JavaScript class using tree-sitter structural placement. Idempotent when a method with the same name already exists; unsupported languages fail clearly."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Source file" },
                "class_or_struct": { "type": "string", "description": "Rust impl target type or class name" },
                "method_source": { "type": "string", "description": "Method source without class/impl indentation; inserted structurally into the target" }
            },
            "required": ["path", "class_or_struct", "method_source"]
        })
    }

    async fn call(
        &self,
        params: Value,
        _cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: AddMethodParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let path = p.path.as_str();
        let pb = PathBuf::from(path);
        let lang = match Language::from_path(&pb) {
            Some(l) => l,
            None => {
                return Ok(ToolResult::err(format!(
                    "Unsupported file type for add_method: {path}"
                )));
            }
        };
        let source = tokio::fs::read_to_string(path).await?;
        match add_method_to_source(lang, &source, &p.class_or_struct, &p.method_source)? {
            StructuralEdit::Changed(new_source) => {
                tokio::fs::write(path, &new_source).await?;
                Ok(edit_result(
                    "add_method",
                    path,
                    &source,
                    &new_source,
                    format!("Added method to `{}` in {path}", p.class_or_struct),
                ))
            }
            StructuralEdit::Unchanged(msg) => Ok(ToolResult::ok(msg)),
            StructuralEdit::Unsupported(msg) | StructuralEdit::Error(msg) => {
                Ok(ToolResult::err(msg))
            }
        }
    }
}

#[derive(Clone)]
pub struct AddImportTool;

#[async_trait]
impl Tool for AddImportTool {
    fn name(&self) -> &str {
        "add_import"
    }

    fn description(&self) -> &str {
        "Add an import/use statement with deterministic insertion and sorting for Rust, Python, TypeScript, and JavaScript. Re-running an existing import is a no-op; unsupported languages fail clearly."
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Source file" },
                "import_statement": { "type": "string", "description": "Complete import/use statement, e.g. `use crate::x;` or `import os`" }
            },
            "required": ["path", "import_statement"]
        })
    }

    async fn call(
        &self,
        params: Value,
        _cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: AddImportParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let path = p.path.as_str();
        let pb = PathBuf::from(path);
        let lang = match Language::from_path(&pb) {
            Some(l) => l,
            None => {
                return Ok(ToolResult::err(format!(
                    "Unsupported file type for add_import: {path}"
                )));
            }
        };
        let source = tokio::fs::read_to_string(path).await?;
        match add_import_to_source(lang, &source, &p.import_statement) {
            StructuralEdit::Changed(new_source) => {
                tokio::fs::write(path, &new_source).await?;
                Ok(edit_result(
                    "add_import",
                    path,
                    &source,
                    &new_source,
                    format!("Added import to {path}"),
                ))
            }
            StructuralEdit::Unchanged(msg) => Ok(ToolResult::ok(msg)),
            StructuralEdit::Unsupported(msg) | StructuralEdit::Error(msg) => {
                Ok(ToolResult::err(msg))
            }
        }
    }
}

#[derive(Clone)]
pub struct RenameSymbolTool {
    manager: Arc<LspManager>,
}

impl RenameSymbolTool {
    pub fn new(manager: Arc<LspManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for RenameSymbolTool {
    fn name(&self) -> &str {
        "rename_symbol"
    }
    fn description(&self) -> &str {
        "Rename a symbol across the workspace via LSP `textDocument/rename`, apply the resulting textual workspace edits safely, and surface a diff-friendly summary. Resource operations such as create/rename/delete are rejected. Requires an LSP server for the language."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "line": { "type": "integer", "description": "1-indexed source line" },
                "character": { "type": "integer", "description": "0-indexed column" },
                "new_name": { "type": "string" }
            },
            "required": ["path", "line", "character", "new_name"]
        })
    }
    async fn call(
        &self,
        params: Value,
        _cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: RenameSymbolParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let path = p.path.as_str();
        let line = p.line;
        let character = p.character;
        let new_name = p.new_name.as_str();
        let pb = PathBuf::from(path);
        let lang = match Language::from_path(&pb) {
            Some(l) => l,
            None => return Ok(ToolResult::err(format!("Unsupported file type: {path}"))),
        };
        let server = match self.manager.server(lang).await {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolResult::err(format!(
                    "LSP unavailable for {}: {e}",
                    lang.name()
                )));
            }
        };

        // didOpen so the server has the file contents indexed.
        let source = tokio::fs::read_to_string(path).await?;
        server.did_open(&pb, &source).await?;

        let line0 = (line as u32).saturating_sub(1);
        let request = json!({
            "textDocument": { "uri": format!("file://{}", absolute_path(&pb)?) },
            "position": { "line": line0, "character": character },
            "newName": new_name,
        });
        let resp: Value = server
            .request("textDocument/rename", request)
            .await
            .map_err(|e| anyhow::anyhow!("rename request failed: {e}"))?;
        if resp.is_null() {
            return Ok(ToolResult::ok(format!(
                "LSP rename returned no edits for `{}` in {}",
                new_name, path
            )));
        }

        let workspace_edit: WorkspaceEdit = serde_json::from_value(resp.clone())
            .map_err(|e| anyhow!("rename response was not a valid WorkspaceEdit: {e}"))?;
        let workspace_root = std::env::current_dir()?;
        let primary_path = workspace_file_path(&workspace_root, &pb)?;
        let applied = apply_workspace_edit(&workspace_root, &pb, workspace_edit).await?;
        if applied.is_empty() {
            return Ok(ToolResult::ok(format!(
                "LSP rename returned an empty WorkspaceEdit for `{}` in {}",
                new_name, path
            ))
            .with_details(json!({
                "path": path,
                "new_name": new_name,
                "workspace_edit": resp,
                "files": [],
                "edit_count": 0,
            })));
        }

        let total_edits = applied.iter().map(|file| file.edit_count).sum::<usize>();
        let primary_file = applied
            .iter()
            .find(|file| file.path == primary_path)
            .unwrap_or(&applied[0]);
        let diff = EditDiff {
            path: primary_file.path.display().to_string(),
            tool: "rename_symbol".into(),
            before: crate::tools::snippet(&primary_file.before, 6, 800),
            after: crate::tools::snippet(&primary_file.after, 6, 800),
            at: chrono::Local::now(),
        };
        let details = json!({
            "path": path,
            "new_name": new_name,
            "workspace_edit": resp,
            "edit_count": total_edits,
            "files": applied.iter().map(|file| json!({
                "path": file.path.display().to_string(),
                "edit_count": file.edit_count,
            })).collect::<Vec<_>>(),
        });
        let output = format!(
            "Renamed symbol to `{}` across {} file(s) with {} edit(s)",
            new_name,
            applied.len(),
            total_edits
        );
        Ok(ToolResult::ok(output)
            .with_details(details)
            .with_display_preview(workspace_edit_preview(&applied))
            .with_edit_diff(diff))
    }
}

fn absolute_path(p: &PathBuf) -> Result<String> {
    let abs = if p.is_absolute() {
        p.clone()
    } else {
        std::env::current_dir()?.join(p)
    };
    Ok(abs.to_string_lossy().into_owned())
}

#[derive(Debug, Clone)]
struct AppliedFileEdit {
    path: PathBuf,
    before: String,
    after: String,
    edit_count: usize,
}

#[derive(Debug)]
struct PlannedTextEdit {
    start: usize,
    end: usize,
    new_text: String,
    order: usize,
}

fn uri_to_path(uri: &Uri) -> Result<PathBuf> {
    let raw = uri.to_string();
    let Some(path) = raw.strip_prefix("file://") else {
        bail!("workspace edit only supports file:// URIs, got {raw}");
    };
    Ok(PathBuf::from(path))
}

fn canonicalize_for_workspace_check(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return std::fs::canonicalize(path)
            .with_context(|| format!("canonicalize {}", path.display()));
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path {} has no parent", path.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("path {} has no file name", path.display()))?;
    Ok(std::fs::canonicalize(parent)
        .with_context(|| format!("canonicalize parent {}", parent.display()))?
        .join(name))
}

fn workspace_file_path(workspace_root: &Path, path: &Path) -> Result<PathBuf> {
    let workspace_root = std::fs::canonicalize(workspace_root)
        .with_context(|| format!("canonicalize workspace root {}", workspace_root.display()))?;
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };
    let canonical = canonicalize_for_workspace_check(&abs)?;
    if !canonical.starts_with(&workspace_root) {
        bail!(
            "workspace edit path {} escapes workspace root {}",
            canonical.display(),
            workspace_root.display()
        );
    }
    Ok(canonical)
}

fn byte_offset_for_position(source: &str, position: Position) -> Result<usize> {
    let mut line_start = 0usize;
    for line_idx in 0..position.line {
        let rel = source[line_start..].find('\n').ok_or_else(|| {
            anyhow!(
                "line {} out of range while resolving workspace edit position",
                line_idx + 1
            )
        })?;
        line_start += rel + 1;
    }
    let line_end = source[line_start..]
        .find('\n')
        .map(|idx| line_start + idx)
        .unwrap_or(source.len());
    let line = &source[line_start..line_end];
    let mut utf16_units = 0u32;
    for (byte_idx, ch) in line.char_indices() {
        if utf16_units == position.character {
            return Ok(line_start + byte_idx);
        }
        utf16_units += ch.len_utf16() as u32;
        if utf16_units > position.character {
            bail!(
                "workspace edit character {} splits a UTF-16 code point on line {}",
                position.character,
                position.line + 1
            );
        }
    }
    if utf16_units == position.character {
        Ok(line_end)
    } else {
        bail!(
            "workspace edit character {} is beyond line {}",
            position.character,
            position.line + 1
        )
    }
}

fn apply_text_edits(source: &str, edits: &[TextEdit]) -> Result<String> {
    let mut planned = Vec::with_capacity(edits.len());
    for (order, edit) in edits.iter().enumerate() {
        let start = byte_offset_for_position(source, edit.range.start)?;
        let end = byte_offset_for_position(source, edit.range.end)?;
        if end < start {
            bail!("workspace edit range end precedes start");
        }
        planned.push(PlannedTextEdit {
            start,
            end,
            new_text: edit.new_text.clone(),
            order,
        });
    }
    planned.sort_by(|a, b| {
        b.start
            .cmp(&a.start)
            .then_with(|| b.end.cmp(&a.end))
            .then_with(|| b.order.cmp(&a.order))
    });

    let mut rendered = source.to_string();
    let mut next_limit = source.len();
    for edit in planned {
        if edit.end > next_limit {
            bail!("workspace edit contains overlapping text edits");
        }
        rendered.replace_range(edit.start..edit.end, &edit.new_text);
        next_limit = edit.start;
    }
    Ok(rendered)
}

fn push_document_edits(
    edits_by_path: &mut std::collections::BTreeMap<PathBuf, Vec<TextEdit>>,
    workspace_root: &Path,
    document_edit: TextDocumentEdit,
) -> Result<()> {
    let path = workspace_file_path(
        workspace_root,
        &uri_to_path(&document_edit.text_document.uri)?,
    )?;
    let edits = edits_by_path.entry(path).or_default();
    edits.extend(document_edit.edits.into_iter().map(|edit| match edit {
        OneOf::Left(edit) => edit,
        OneOf::Right(AnnotatedTextEdit { text_edit, .. }) => text_edit,
    }));
    Ok(())
}

async fn apply_workspace_edit(
    workspace_root: &Path,
    primary_path: &Path,
    workspace_edit: WorkspaceEdit,
) -> Result<Vec<AppliedFileEdit>> {
    let primary_path = workspace_file_path(workspace_root, primary_path)?;
    let mut edits_by_path = std::collections::BTreeMap::<PathBuf, Vec<TextEdit>>::new();

    if let Some(changes) = workspace_edit.changes {
        for (uri, edits) in changes {
            let path = workspace_file_path(workspace_root, &uri_to_path(&uri)?)?;
            edits_by_path.entry(path).or_default().extend(edits);
        }
    }

    if let Some(document_changes) = workspace_edit.document_changes {
        match document_changes {
            DocumentChanges::Edits(document_edits) => {
                for document_edit in document_edits {
                    push_document_edits(&mut edits_by_path, workspace_root, document_edit)?;
                }
            }
            DocumentChanges::Operations(operations) => {
                for operation in operations {
                    match operation {
                        DocumentChangeOperation::Edit(document_edit) => {
                            push_document_edits(&mut edits_by_path, workspace_root, document_edit)?;
                        }
                        DocumentChangeOperation::Op(_) => {
                            bail!("resource operations are not supported in rename workspace edits")
                        }
                    }
                }
            }
        }
    }

    let mut planned = Vec::with_capacity(edits_by_path.len());
    for (path, edits) in edits_by_path {
        let before = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("read {}", path.display()))?;
        let after = apply_text_edits(&before, &edits)
            .with_context(|| format!("apply workspace edits to {}", path.display()))?;
        planned.push(AppliedFileEdit {
            path,
            before,
            after,
            edit_count: edits.len(),
        });
    }

    for file in &planned {
        if file.before != file.after {
            tokio::fs::write(&file.path, &file.after)
                .await
                .with_context(|| format!("write {}", file.path.display()))?;
        }
    }

    planned.sort_by(|a, b| {
        (a.path != primary_path)
            .cmp(&(b.path != primary_path))
            .then_with(|| a.path.cmp(&b.path))
    });
    Ok(planned)
}

fn diff_preview(before: &str, after: &str, label: &str) -> String {
    let max_lines = 10;
    let max_chars = 1024;
    let mut out = String::new();
    out.push_str(label);
    out.push('\n');
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let mut emitted = 0;
    for (i, line) in before_lines.iter().enumerate() {
        if emitted >= max_lines || out.len() >= max_chars {
            break;
        }
        if after_lines.get(i).copied() != Some(*line) {
            out.push_str(&format!("- {line}\n"));
            emitted += 1;
        }
    }
    for (i, line) in after_lines.iter().enumerate() {
        if emitted >= max_lines || out.len() >= max_chars {
            break;
        }
        if before_lines.get(i).copied() != Some(*line) {
            out.push_str(&format!("+ {line}\n"));
            emitted += 1;
        }
    }
    let total_changes = before_lines
        .iter()
        .enumerate()
        .filter(|(i, line)| after_lines.get(*i).copied() != Some(**line))
        .count()
        + after_lines
            .iter()
            .enumerate()
            .filter(|(i, line)| before_lines.get(*i).copied() != Some(**line))
            .count();
    if emitted < total_changes {
        out.push_str(&format!("… {} more change(s)\n", total_changes - emitted));
    }
    out
}

fn workspace_edit_preview(applied: &[AppliedFileEdit]) -> String {
    let mut previews = applied
        .iter()
        .take(3)
        .map(|file| {
            diff_preview(
                &file.before,
                &file.after,
                &format!("EDITED · {}", file.path.display()),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if applied.len() > 3 {
        previews.push_str(&format!("\n… {} more file(s) changed", applied.len() - 3));
    }
    previews
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{
        DocumentChangeOperation, DocumentChanges, OptionalVersionedTextDocumentIdentifier,
        Position, Range, TextEdit, WorkspaceEdit,
    };
    use tempfile::tempdir;

    #[tokio::test]
    async fn replace_function_body_swaps_block_only() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "fn alpha() { 1 }\n\nfn beta() {\n    1\n}\n").unwrap();
        let tool = ReplaceFunctionBodyTool;
        let result = tool
            .call(
                json!({
                    "path": path.to_string_lossy(),
                    "symbol": "beta",
                    "new_body": "{\n    42\n}"
                }),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.output);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("fn alpha() { 1 }"), "got {after}");
        assert!(after.contains("fn beta() {\n    42\n}"), "got {after}");
        assert!(!after.contains("fn beta() {\n    1\n}"), "got {after}");
    }

    #[tokio::test]
    async fn replace_function_body_missing_symbol_errors_clearly() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "fn alpha() {}\n").unwrap();
        let result = ReplaceFunctionBodyTool
            .call(
                json!({
                    "path": path.to_string_lossy(),
                    "symbol": "ghost",
                    "new_body": "{}"
                }),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("ghost"), "got {}", result.output);
        assert!(
            result.output.contains("code_outline"),
            "should hint at code_outline: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn apply_workspace_edit_changes_map_applies_multifile_rename() {
        let dir = tempdir().unwrap();
        let main = dir.path().join("main.rs");
        let helper = dir.path().join("helper.rs");
        std::fs::write(&main, "fn old_name() {}\nfn call() { old_name(); }\n").unwrap();
        std::fs::write(&helper, "pub fn bridge() { crate::old_name(); }\n").unwrap();

        let edit = WorkspaceEdit {
            changes: Some(std::collections::HashMap::from([
                (
                    format!("file://{}", main.display()).parse().unwrap(),
                    vec![
                        TextEdit {
                            range: Range {
                                start: Position {
                                    line: 0,
                                    character: 3,
                                },
                                end: Position {
                                    line: 0,
                                    character: 11,
                                },
                            },
                            new_text: "new_name".into(),
                        },
                        TextEdit {
                            range: Range {
                                start: Position {
                                    line: 1,
                                    character: 12,
                                },
                                end: Position {
                                    line: 1,
                                    character: 20,
                                },
                            },
                            new_text: "new_name".into(),
                        },
                    ],
                ),
                (
                    format!("file://{}", helper.display()).parse().unwrap(),
                    vec![TextEdit {
                        range: Range {
                            start: Position {
                                line: 0,
                                character: 25,
                            },
                            end: Position {
                                line: 0,
                                character: 33,
                            },
                        },
                        new_text: "new_name".into(),
                    }],
                ),
            ])),
            document_changes: None,
            change_annotations: None,
        };

        let applied = apply_workspace_edit(dir.path(), &main, edit).await.unwrap();

        assert_eq!(applied.len(), 2);
        assert_eq!(
            std::fs::read_to_string(&main).unwrap(),
            "fn new_name() {}\nfn call() { new_name(); }\n"
        );
        assert_eq!(
            std::fs::read_to_string(&helper).unwrap(),
            "pub fn bridge() { crate::new_name(); }\n"
        );
        assert_eq!(applied.iter().map(|f| f.edit_count).sum::<usize>(), 3);
    }

    #[tokio::test]
    async fn apply_workspace_edit_document_changes_edits_applies_annotated_text_edits() {
        let dir = tempdir().unwrap();
        let main = dir.path().join("main.rs");
        let helper = dir.path().join("helper.rs");
        std::fs::write(&main, "fn old_name() {}\nfn call() { old_name(); }\n").unwrap();
        std::fs::write(&helper, "pub fn bridge() { crate::old_name(); }\n").unwrap();

        let edit = WorkspaceEdit {
            changes: None,
            document_changes: Some(DocumentChanges::Edits(vec![
                TextDocumentEdit {
                    text_document: OptionalVersionedTextDocumentIdentifier {
                        uri: format!("file://{}", main.display()).parse().unwrap(),
                        version: None,
                    },
                    edits: vec![
                        OneOf::Left(TextEdit {
                            range: Range {
                                start: Position {
                                    line: 0,
                                    character: 3,
                                },
                                end: Position {
                                    line: 0,
                                    character: 11,
                                },
                            },
                            new_text: "new_name".into(),
                        }),
                        OneOf::Right(AnnotatedTextEdit {
                            text_edit: TextEdit {
                                range: Range {
                                    start: Position {
                                        line: 1,
                                        character: 12,
                                    },
                                    end: Position {
                                        line: 1,
                                        character: 20,
                                    },
                                },
                                new_text: "new_name".into(),
                            },
                            annotation_id: "rename".into(),
                        }),
                    ],
                },
                TextDocumentEdit {
                    text_document: OptionalVersionedTextDocumentIdentifier {
                        uri: format!("file://{}", helper.display()).parse().unwrap(),
                        version: None,
                    },
                    edits: vec![OneOf::Left(TextEdit {
                        range: Range {
                            start: Position {
                                line: 0,
                                character: 25,
                            },
                            end: Position {
                                line: 0,
                                character: 33,
                            },
                        },
                        new_text: "new_name".into(),
                    })],
                },
            ])),
            change_annotations: None,
        };

        let applied = apply_workspace_edit(dir.path(), &main, edit).await.unwrap();

        assert_eq!(applied.len(), 2);
        assert_eq!(
            std::fs::read_to_string(&main).unwrap(),
            "fn new_name() {}\nfn call() { new_name(); }\n"
        );
        assert_eq!(
            std::fs::read_to_string(&helper).unwrap(),
            "pub fn bridge() { crate::new_name(); }\n"
        );
        assert_eq!(applied.iter().map(|f| f.edit_count).sum::<usize>(), 3);
    }

    #[tokio::test]
    async fn apply_workspace_edit_rejects_unsupported_resource_ops_without_writing() {
        let dir = tempdir().unwrap();
        let main = dir.path().join("main.rs");
        std::fs::write(&main, "fn old_name() {}\n").unwrap();
        let before = std::fs::read_to_string(&main).unwrap();

        let edit = WorkspaceEdit {
            changes: None,
            document_changes: Some(DocumentChanges::Operations(vec![
                DocumentChangeOperation::Op(lsp_types::ResourceOp::Delete(lsp_types::DeleteFile {
                    uri: format!("file://{}", main.display()).parse().unwrap(),
                    options: None,
                })),
            ])),
            change_annotations: None,
        };

        let err = apply_workspace_edit(dir.path(), &main, edit)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("resource operations are not supported"),
            "got {err}"
        );
        assert_eq!(std::fs::read_to_string(&main).unwrap(), before);
    }

    #[tokio::test]
    async fn add_method_inserts_into_rust_impl_and_is_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(
            &path,
            "struct Widget;\n\nimpl Widget {\n    fn existing(&self) -> i32 {\n        1\n    }\n}\n",
        )
        .unwrap();
        let tool = AddMethodTool;
        let params = json!({
            "path": path.to_string_lossy(),
            "class_or_struct": "Widget",
            "method_source": "pub fn added(&self) -> i32 {\n    42\n}"
        });

        let result = tool
            .call(params.clone(), CancellationToken::new(), None)
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.output);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("    fn existing(&self) -> i32"),
            "got {after}"
        );
        assert!(
            after.contains("    pub fn added(&self) -> i32 {\n        42\n    }"),
            "got {after}"
        );

        let second = tool
            .call(params, CancellationToken::new(), None)
            .await
            .unwrap();
        assert!(!second.is_error, "{}", second.output);
        let after_second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, after_second, "add_method should be idempotent");
        assert!(
            second.output.contains("already exists"),
            "got {}",
            second.output
        );
    }

    #[tokio::test]
    async fn add_method_inserts_into_python_class() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(
            &path,
            "class Widget:\n    def existing(self):\n        return 1\n",
        )
        .unwrap();
        let result = AddMethodTool
            .call(
                json!({
                    "path": path.to_string_lossy(),
                    "class_or_struct": "Widget",
                    "method_source": "def added(self):\n    return 42"
                }),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.output);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("    def existing(self):\n        return 1\n\n    def added(self):\n        return 42\n"), "got {after}");
    }

    #[tokio::test]
    async fn add_method_missing_rust_impl_errors_clearly() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "struct Widget;\n").unwrap();
        let result = AddMethodTool
            .call(
                json!({
                    "path": path.to_string_lossy(),
                    "class_or_struct": "Widget",
                    "method_source": "fn added(&self) {}"
                }),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("impl"), "got {}", result.output);
        assert!(result.output.contains("Widget"), "got {}", result.output);
    }

    #[tokio::test]
    async fn add_import_inserts_sorted_rust_use_and_is_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        std::fs::write(&path, "use crate::zeta;\n\nfn main() {}\n").unwrap();
        let tool = AddImportTool;
        let params = json!({
            "path": path.to_string_lossy(),
            "import_statement": "use crate::alpha;"
        });
        let result = tool
            .call(params.clone(), CancellationToken::new(), None)
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.output);
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            after,
            "use crate::alpha;\nuse crate::zeta;\n\nfn main() {}\n"
        );

        let second = tool
            .call(params, CancellationToken::new(), None)
            .await
            .unwrap();
        assert!(!second.is_error, "{}", second.output);
        assert_eq!(after, std::fs::read_to_string(&path).unwrap());
        assert!(
            second.output.contains("already exists"),
            "got {}",
            second.output
        );
    }

    #[tokio::test]
    async fn add_import_places_python_import_after_docstring() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.py");
        std::fs::write(&path, "#!/usr/bin/env python3\n\"\"\"module docs\"\"\"\n\nimport zeta\n\ndef main():\n    pass\n").unwrap();
        let result = AddImportTool
            .call(
                json!({
                    "path": path.to_string_lossy(),
                    "import_statement": "import alpha"
                }),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.output);
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            after,
            "#!/usr/bin/env python3\n\"\"\"module docs\"\"\"\n\nimport alpha\nimport zeta\n\ndef main():\n    pass\n"
        );
    }

    #[tokio::test]
    async fn add_import_unsupported_language_errors_without_writing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.go");
        std::fs::write(&path, "package main\n\nfunc main() {}\n").unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let result = AddImportTool
            .call(
                json!({
                    "path": path.to_string_lossy(),
                    "import_statement": "import \"fmt\""
                }),
                CancellationToken::new(),
                None,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(
            result.output.contains("not supported"),
            "got {}",
            result.output
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }
}
