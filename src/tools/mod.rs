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
}

impl ToolResult {
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            media: Vec::new(),
            is_error: false,
        }
    }

    pub fn err(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            media: Vec::new(),
            is_error: true,
        }
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
}

pub mod file;
pub mod shell;
pub mod web;

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
pub type EditDiffSink = Arc<std::sync::Mutex<Option<EditDiff>>>;

pub fn new_diff_sink() -> EditDiffSink {
    Arc::new(std::sync::Mutex::new(None))
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
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::new_with_diff_sink(None)
    }

    /// Build a tool registry that wires `WriteFile`/`PatchFile` to a
    /// shared diff sink (YYC-66). Pass `Some(sink)` to capture edits;
    /// `None` keeps the legacy behavior (tools don't observe their own
    /// writes).
    pub fn new_with_diff_sink(sink: Option<EditDiffSink>) -> Self {
        let mut registry = Self {
            tools: HashMap::new(),
        };
        registry.register(Arc::new(file::ReadFile));
        registry.register(Arc::new(file::WriteFile::new(sink.clone())));
        registry.register(Arc::new(file::SearchFiles));
        registry.register(Arc::new(file::PatchFile::new(sink)));
        registry.register(Arc::new(web::WebSearch));
        registry.register(Arc::new(web::WebFetch));
        for tool in shell::make_tools() {
            registry.register(tool);
        }
        registry
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Get all tool definitions for the LLM
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|t| ToolDefinition {
                tool_type: "function".into(),
                function: crate::provider::ToolFunction {
                    name: t.name().to_string(),
                    description: t.description().to_string(),
                    parameters: t.schema(),
                },
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
    use tokio_util::sync::CancellationToken;

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
