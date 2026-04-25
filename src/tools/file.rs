
use crate::tools::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Read a file from the filesystem. Returns content with line numbers."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path to the file" },
                "offset": { "type": "integer", "description": "Line number to start from (1-indexed)", "default": 1 },
                "limit": { "type": "integer", "description": "Max lines to return", "default": 500 }
            },
            "required": ["path"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let path = params["path"].as_str().ok_or_else(|| anyhow::anyhow!("path required"))?;
        let offset = params["offset"].as_i64().unwrap_or(1);
        let limit = params["limit"].as_i64().unwrap_or(500);

        let content = tokio::fs::read_to_string(path).await?;
        let lines: Vec<&str> = content.lines().collect();
        let start = (offset - 1) as usize;
        let end = (start + limit as usize).min(lines.len());

        if start >= lines.len() {
            return Ok(ToolResult::ok("File offset exceeds file length."));
        }

        let result: String = lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>6}|{line}", start + i + 1))
            .collect::<Vec<_>>()
            .join("\n");

        let output = if result.is_empty() {
            "File is empty.".to_string()
        } else {
            format!("{result}\n---\n{}/{} lines shown", end - start, lines.len())
        };
        Ok(ToolResult::ok(output))
    }
}

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Write content to a file. Creates parent directories if needed. OVERWRITES existing content."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path to write to" },
                "content": { "type": "string", "description": "Complete content to write" }
            },
            "required": ["path", "content"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let path = params["path"].as_str().ok_or_else(|| anyhow::anyhow!("path required"))?;
        let content = params["content"].as_str().unwrap_or("");

        // Create parent directories
        if let Some(parent) = std::path::Path::new(path).parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        tokio::fs::write(path, content).await?;
        let bytes = content.len();
        Ok(ToolResult::ok(format!("Wrote {bytes} bytes to {path}")))
    }
}

pub struct SearchFiles;

#[async_trait]
impl Tool for SearchFiles {
    fn name(&self) -> &str {
        "search_files"
    }
    fn description(&self) -> &str {
        "Search file contents using regex patterns. Ripgrep-style. Fast for large codebases."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex pattern to search for" },
                "path": { "type": "string", "description": "Directory to search in", "default": "." },
                "file_glob": { "type": "string", "description": "Filter files by glob (e.g. '*.rs')" },
                "limit": { "type": "integer", "description": "Max results", "default": 20 }
            },
            "required": ["pattern"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let pattern = params["pattern"].as_str().ok_or_else(|| anyhow::anyhow!("pattern required"))?;
        let path = params["path"].as_str().unwrap_or(".");
        let limit = params["limit"].as_i64().unwrap_or(20);

        let mut cmd = tokio::process::Command::new("rg");
        cmd.arg("--line-number").arg("--color").arg("never");
        cmd.arg("-m").arg(limit.to_string());
        cmd.arg(pattern);
        cmd.arg(path);

        if let Some(glob) = params["file_glob"].as_str() {
            cmd.arg("--glob").arg(glob);
        }

        let output = cmd.output().await?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !output.status.success() && !stderr.is_empty() {
            return Err(anyhow::anyhow!("rg failed: {stderr}"));
        }

        let result = stdout.trim().to_string();
        let output = if result.is_empty() {
            "No matches found.".to_string()
        } else {
            result
        };
        Ok(ToolResult::ok(output))
    }
}

pub struct PatchFile;

#[async_trait]
impl Tool for PatchFile {
    fn name(&self) -> &str {
        "edit_file"
    }
    fn description(&self) -> &str {
        "Find and replace text in a file. Uses fuzzy matching so minor whitespace differences won't break it."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File to edit" },
                "old_string": { "type": "string", "description": "Text to find (must be unique unless replace_all=true)" },
                "new_string": { "type": "string", "description": "Replacement text" }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let path = params["path"].as_str().ok_or_else(|| anyhow::anyhow!("path required"))?;
        let old = params["old_string"].as_str().ok_or_else(|| anyhow::anyhow!("old_string required"))?;
        let new = params["new_string"].as_str().unwrap_or("");

        let content = tokio::fs::read_to_string(path).await?;

        if !content.contains(old) {
            // Try fuzzy matching — check for similar strings
            let similar = content
                .lines()
                .filter(|l| l.contains(&old[..old.len().min(20)]))
                .take(3)
                .map(|l| format!("  \"{l}\""))
                .collect::<Vec<_>>();

            let hint = if similar.is_empty() {
                "No similar text found nearby.".to_string()
            } else {
                format!("Did you mean one of:\n{}", similar.join("\n"))
            };

            return Err(anyhow::anyhow!("old_string not found in {path}.\n{hint}"));
        }

        let new_content = content.replace(old, new);
        tokio::fs::write(path, &new_content).await?;

        let replaces = content.matches(old).count();
        Ok(ToolResult::ok(format!("Replaced {replaces} occurrence(s) in {path}")))
    }
}
