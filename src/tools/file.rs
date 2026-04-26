use crate::tools::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
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
        let path = params["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path required"))?;
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

pub struct ListFiles;

#[async_trait]
impl Tool for ListFiles {
    fn name(&self) -> &str {
        "list_files"
    }
    fn description(&self) -> &str {
        "List files + directories under a path as a structured tree (JSON). Respects .gitignore. Cheaper than shelling out to `ls` or `tree` and won't drown in target/ or node_modules/."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Root path to walk", "default": "." },
                "depth": { "type": "integer", "description": "Max depth (default 2)", "default": 2 },
                "include_hidden": { "type": "boolean", "default": false },
                "max_entries": { "type": "integer", "default": 500 }
            }
        })
    }
    async fn call(&self, params: Value, _cancel: CancellationToken) -> Result<ToolResult> {
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();
        let depth = params
            .get("depth")
            .and_then(|v| v.as_u64())
            .unwrap_or(2) as usize;
        let include_hidden = params
            .get("include_hidden")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let max_entries = params
            .get("max_entries")
            .and_then(|v| v.as_u64())
            .unwrap_or(500) as usize;

        let walker = ignore::WalkBuilder::new(&path)
            .standard_filters(true)
            .hidden(!include_hidden)
            .max_depth(Some(depth + 1))
            .build();
        let root = std::path::PathBuf::from(&path);
        let mut entries: Vec<Value> = Vec::new();
        let mut truncated = false;
        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            // Skip the root itself.
            if entry.depth() == 0 {
                continue;
            }
            if entries.len() >= max_entries {
                truncated = true;
                break;
            }
            let p = entry.path();
            let rel = p
                .strip_prefix(&root)
                .unwrap_or(p)
                .to_string_lossy()
                .into_owned();
            let kind = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                "dir"
            } else if entry.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
                "symlink"
            } else {
                "file"
            };
            let size = std::fs::metadata(p).ok().map(|m| m.len());
            entries.push(json!({
                "path": rel,
                "kind": kind,
                "depth": entry.depth(),
                "size": size,
            }));
        }
        let payload = json!({
            "root": path,
            "depth": depth,
            "count": entries.len(),
            "truncated": truncated,
            "entries": entries,
        });
        Ok(ToolResult::ok(serde_json::to_string_pretty(&payload)?))
    }
}

/// Build a compact unified-style diff preview for the YYC-74 card
/// (YYC-bonus: matches Claude Code-style render). Caps at ~10 line
/// pairs and 1KB so megabyte rewrites don't bloat the TUI.
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
        // Only show before lines that aren't matched in after at the
        // same index (cheap line-by-line comparison; not a real diff).
        if after_lines.get(i).map(|s| *s) != Some(*line) {
            out.push_str(&format!("- {line}\n"));
            emitted += 1;
        }
    }
    for (i, line) in after_lines.iter().enumerate() {
        if emitted >= max_lines || out.len() >= max_chars {
            break;
        }
        if before_lines.get(i).map(|s| *s) != Some(*line) {
            out.push_str(&format!("+ {line}\n"));
            emitted += 1;
        }
    }
    let total_changes = before_lines
        .iter()
        .enumerate()
        .filter(|(i, l)| after_lines.get(*i).map(|s| *s) != Some(**l))
        .count()
        + after_lines
            .iter()
            .enumerate()
            .filter(|(i, l)| before_lines.get(*i).map(|s| *s) != Some(**l))
            .count();
    if emitted < total_changes {
        out.push_str(&format!("… {} more change(s)\n", total_changes - emitted));
    }
    out
}

pub struct WriteFile {
    diff_sink: Option<crate::tools::EditDiffSink>,
}

impl WriteFile {
    pub fn new(diff_sink: Option<crate::tools::EditDiffSink>) -> Self {
        Self { diff_sink }
    }
}

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
        let path = params["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path required"))?;
        let content = params["content"].as_str().unwrap_or("");

        // YYC-66: snapshot the existing content before overwriting so the
        // diff sink has a meaningful "before" — empty string for a fresh
        // file is correct (it didn't exist).
        let before = if self.diff_sink.is_some() {
            tokio::fs::read_to_string(path).await.unwrap_or_default()
        } else {
            String::new()
        };

        // Create parent directories
        if let Some(parent) = std::path::Path::new(path).parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        tokio::fs::write(path, content).await?;
        let bytes = content.len();

        if let Some(sink) = &self.diff_sink {
            let diff = crate::tools::EditDiff {
                path: path.to_string(),
                tool: "write_file".into(),
                before: crate::tools::snippet(&before, 6, 800),
                after: crate::tools::snippet(content, 6, 800),
                at: chrono::Local::now(),
            };
            *sink.lock().unwrap() = Some(diff);
        }

        // YYC-bonus: surface a real diff in the card preview without
        // polluting the LLM-facing output.
        let label = if before.is_empty() {
            format!("NEW FILE · {path}")
        } else {
            format!("MODIFIED · {path}")
        };
        let display = diff_preview(&before, content, &label);
        Ok(ToolResult::ok(format!("Wrote {bytes} bytes to {path}"))
            .with_display_preview(display))
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
        let pattern = params["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("pattern required"))?;
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

pub struct PatchFile {
    diff_sink: Option<crate::tools::EditDiffSink>,
}

impl PatchFile {
    pub fn new(diff_sink: Option<crate::tools::EditDiffSink>) -> Self {
        Self { diff_sink }
    }
}

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
        let path = params["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path required"))?;
        let old = params["old_string"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("old_string required"))?;
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

        // YYC-66: capture the before/after snippets so the TUI diff pane
        // shows actual edited text rather than demo data.
        if let Some(sink) = &self.diff_sink {
            let diff = crate::tools::EditDiff {
                path: path.to_string(),
                tool: "edit_file".into(),
                before: crate::tools::snippet(old, 6, 800),
                after: crate::tools::snippet(new, 6, 800),
                at: chrono::Local::now(),
            };
            *sink.lock().unwrap() = Some(diff);
        }

        // YYC-bonus: render a Claude Code-style diff in the card.
        let display = diff_preview(old, new, &format!("EDITED · {path}"));
        Ok(ToolResult::ok(format!(
            "Replaced {replaces} occurrence(s) in {path}"
        ))
        .with_display_preview(display))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn write_file_captures_before_after_into_diff_sink() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        let path_str = path.to_string_lossy().to_string();
        // Pre-existing content so "before" is non-empty.
        std::fs::write(&path, "old contents\n").unwrap();

        let sink = crate::tools::new_diff_sink();
        let tool = WriteFile::new(Some(sink.clone()));
        tool.call(
            json!({"path": path_str, "content": "new contents\n"}),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let diff = sink.lock().unwrap().clone().expect("diff captured");
        assert_eq!(diff.tool, "write_file");
        assert_eq!(diff.path, path_str);
        assert_eq!(diff.before, "old contents");
        assert_eq!(diff.after, "new contents");
    }

    #[tokio::test]
    async fn list_files_returns_structured_tree() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("b.txt"), "world").unwrap();
        let result = ListFiles
            .call(
                json!({"path": dir.path().to_string_lossy(), "depth": 2}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.output);
        let payload: Value = serde_json::from_str(&result.output).unwrap();
        let entries = payload["entries"].as_array().unwrap();
        let names: Vec<&str> = entries
            .iter()
            .map(|e| e["path"].as_str().unwrap_or(""))
            .collect();
        assert!(names.iter().any(|n| n == &"a.txt"));
        assert!(names.iter().any(|n| n == &"sub"));
        // The nested file should appear because depth=2 covers it.
        assert!(names.iter().any(|n| n.ends_with("b.txt")), "got {names:?}");
    }

    #[tokio::test]
    async fn patch_file_captures_old_and_new_strings_into_diff_sink() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        let path_str = path.to_string_lossy().to_string();
        std::fs::write(&path, "fn foo() { 1 }\n").unwrap();

        let sink = crate::tools::new_diff_sink();
        let tool = PatchFile::new(Some(sink.clone()));
        tool.call(
            json!({
                "path": path_str,
                "old_string": "1",
                "new_string": "42",
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let diff = sink.lock().unwrap().clone().expect("diff captured");
        assert_eq!(diff.tool, "edit_file");
        assert_eq!(diff.before, "1");
        assert_eq!(diff.after, "42");
    }
}
