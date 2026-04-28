use crate::pause::{AgentPause, AgentResume, DiffScrubHunk, PauseKind, PauseSender};
use crate::tools::{Tool, ToolResult, fs_sandbox};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

pub struct ReadFile;

/// YYC-159: cap on the in-memory file load. Files larger than this
/// surface a truncated/too-large response without ever calling
/// `read_to_string`, so the agent can't OOM the host while pulling
/// in a multi-GB log or binary. Sized generously (50 MiB) so any
/// realistic source file or text dump still passes.
const READ_FILE_MAX_BYTES: u64 = 50 * 1024 * 1024;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Read a file from the filesystem. Returns content with line numbers. Use this instead of `cat`, `head`, `tail`, or `sed -n` via bash."
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
        use tokio::fs::File;
        use tokio::io::{AsyncBufReadExt, BufReader};

        let path = params["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path required"))?;
        let offset = params["offset"].as_i64().unwrap_or(1);
        let limit = params["limit"].as_i64().unwrap_or(500);

        // YYC-248: refuse reads of system credential / pseudo-fs paths
        // before any I/O happens.
        let path = match fs_sandbox::validate_read(path) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::err(e.to_string())),
        };

        // YYC-159: preflight the file size so multi-GB inputs are
        // refused before they hit `read_to_string` and OOM the host.
        let metadata = tokio::fs::metadata(&path).await?;
        let size = metadata.len();
        if size > READ_FILE_MAX_BYTES {
            let mib = size as f64 / (1024.0 * 1024.0);
            let cap_mib = READ_FILE_MAX_BYTES as f64 / (1024.0 * 1024.0);
            return Ok(ToolResult::ok(format!(
                "File is {size} bytes ({mib:.1} MiB), which exceeds the read_file cap of {cap_mib:.0} MiB. Use grep/ripgrep for searching huge files, or split the read across smaller files."
            )));
        }

        // YYC-199: stream the requested line range instead of
        // pulling the whole file into RAM. A request for
        // offset=1, limit=10 against a 49 MiB log no longer
        // allocates 49 MiB; we only walk far enough to collect
        // `limit` lines and confirm whether more exist after.
        let start = (offset - 1).max(0) as usize;
        let limit = limit.max(0) as usize;
        let target_end = start.saturating_add(limit);

        let file = File::open(&path).await?;
        let reader = BufReader::new(file);
        let mut lines_iter = reader.lines();
        let mut collected: Vec<String> = Vec::with_capacity(limit.min(1024));
        let mut total_seen = 0usize;
        let mut more_after = false;
        while let Some(line) = lines_iter.next_line().await? {
            if total_seen >= target_end {
                more_after = true;
                break;
            }
            if total_seen >= start {
                collected.push(line);
            }
            total_seen += 1;
        }

        if collected.is_empty() && start >= total_seen && !more_after {
            // Empty file or offset beyond EOF.
            if total_seen == 0 {
                return Ok(ToolResult::ok("File is empty."));
            }
            return Ok(ToolResult::ok("File offset exceeds file length."));
        }

        let result: String = collected
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:>6}|{line}", start + i + 1))
            .collect::<Vec<_>>()
            .join("\n");

        let footer = if more_after {
            format!(
                "{}/{}+ lines shown (more available; raise `limit` or use a tighter `offset` window)",
                collected.len(),
                total_seen
            )
        } else {
            format!("{}/{} lines shown", collected.len(), total_seen)
        };
        let output = format!("{result}\n---\n{footer}");
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
        "List files + directories under a path as a structured tree (JSON). Respects .gitignore. Use this instead of `ls`, `tree`, or `find -type f` via bash — won't drown in target/ or node_modules/."
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
        let depth = params.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
        let include_hidden = params
            .get("include_hidden")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let max_entries = params
            .get("max_entries")
            .and_then(|v| v.as_u64())
            .unwrap_or(500) as usize;

        // YYC-248: refuse listing of credential / pseudo-fs roots.
        let path = match fs_sandbox::validate_read(&path) {
            Ok(p) => p.to_string_lossy().into_owned(),
            Err(e) => return Ok(ToolResult::err(e.to_string())),
        };

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
        .filter(|(i, l)| after_lines.get(*i).copied() != Some(**l))
        .count()
        + after_lines
            .iter()
            .enumerate()
            .filter(|(i, l)| before_lines.get(*i).copied() != Some(**l))
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
        "Write content to a file. Creates parent directories if needed. OVERWRITES existing content. Use this instead of `echo > file` or `cat <<EOF` via bash."
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

        // YYC-248: refuse writes to system credential / pseudo-fs paths
        // and to any user-credential or agent-state directory.
        if let Err(e) = fs_sandbox::validate_write(path) {
            return Ok(ToolResult::err(e.to_string()));
        }

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

        // YYC-66 / YYC-131: build the diff record once, then attach it
        // to BOTH the global sink (TUI's last-write panel) and this
        // call's ToolResult (per-call AfterToolCall hook input).
        // Without the per-call attachment, concurrent dispatch could
        // overwrite the sink before DiagnosticsHook sees the matching
        // entry.
        let diff = crate::tools::EditDiff {
            path: path.to_string(),
            tool: "write_file".into(),
            before: crate::tools::snippet(&before, 6, 800),
            after: crate::tools::snippet(content, 6, 800),
            at: chrono::Local::now(),
        };
        if let Some(sink) = &self.diff_sink {
            *sink.lock() = Some(diff.clone());
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
            .with_display_preview(display)
            .with_edit_diff(diff))
    }
}

pub struct SearchFiles;

#[async_trait]
impl Tool for SearchFiles {
    fn name(&self) -> &str {
        "search_files"
    }
    fn description(&self) -> &str {
        "Search file contents using regex patterns. Ripgrep-style, gitignore-aware. Use this instead of `rg`, `grep -r`, or `grep -rn` via bash."
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

        // YYC-248: refuse search roots inside credential / pseudo-fs
        // directories — `rg pattern /etc/shadow` would dump shadow lines
        // matching the pattern to the LLM.
        let path = match fs_sandbox::validate_read(path) {
            Ok(p) => p.to_string_lossy().into_owned(),
            Err(e) => return Ok(ToolResult::err(e.to_string())),
        };

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
    /// Pause channel used to drive the diff scrubber overlay (YYC-75).
    /// When `Some`, `edit_file` calls that match more than one site
    /// route through the scrubber; the user accepts/rejects each hunk
    /// before any bytes hit disk. `None` falls back to the legacy
    /// "replace every match" behavior.
    pause_tx: Option<PauseSender>,
}

impl PatchFile {
    pub fn new(diff_sink: Option<crate::tools::EditDiffSink>) -> Self {
        Self {
            diff_sink,
            pause_tx: None,
        }
    }

    pub fn with_pause(
        diff_sink: Option<crate::tools::EditDiffSink>,
        pause_tx: Option<PauseSender>,
    ) -> Self {
        Self {
            diff_sink,
            pause_tx,
        }
    }
}

#[async_trait]
impl Tool for PatchFile {
    fn name(&self) -> &str {
        "edit_file"
    }
    fn description(&self) -> &str {
        "Find and replace text in a file. Uses fuzzy matching so minor whitespace differences won't break it. Use this instead of `sed -i` via bash."
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

        // YYC-248: edit_file reads then writes — refuse blocked paths
        // for both directions.
        if let Err(e) = fs_sandbox::validate_read(path) {
            return Ok(ToolResult::err(e.to_string()));
        }
        if let Err(e) = fs_sandbox::validate_write(path) {
            return Ok(ToolResult::err(e.to_string()));
        }

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

        // YYC-75: when more than one occurrence and we have a pause
        // channel, send the user through the diff scrubber so they can
        // accept/reject each site individually.
        let occurrences = collect_match_offsets(&content, old);
        let total = occurrences.len();
        let accepted_offsets: Vec<usize> = if let Some(tx) = &self.pause_tx {
            if total > 1 {
                run_diff_scrubber(tx, path, old, new, &content, &occurrences).await?
            } else {
                occurrences
            }
        } else {
            occurrences
        };

        if accepted_offsets.is_empty() {
            return Ok(ToolResult::ok(format!(
                "No hunks accepted — {path} left unchanged."
            )));
        }

        let new_content = apply_accepted_hunks(&content, old, new, &accepted_offsets);
        tokio::fs::write(path, &new_content).await?;
        let replaces = accepted_offsets.len();

        // YYC-66 / YYC-131: build the diff once, attach to both the
        // global sink (TUI's last-write panel) and this call's
        // ToolResult (per-call AfterToolCall hook input). Per-call
        // attachment is what lets DiagnosticsHook react to the right
        // file under concurrent dispatch.
        let diff = crate::tools::EditDiff {
            path: path.to_string(),
            tool: "edit_file".into(),
            before: crate::tools::snippet(old, 6, 800),
            after: crate::tools::snippet(new, 6, 800),
            at: chrono::Local::now(),
        };
        if let Some(sink) = &self.diff_sink {
            *sink.lock() = Some(diff.clone());
        }

        // YYC-bonus: render a Claude Code-style diff in the card.
        let display = diff_preview(old, new, &format!("EDITED · {path}"));
        let msg = if total == replaces {
            format!("Replaced {replaces} occurrence(s) in {path}")
        } else {
            format!("Applied {replaces} of {total} hunk(s) in {path} (others rejected).")
        };
        Ok(ToolResult::ok(msg)
            .with_display_preview(display)
            .with_edit_diff(diff))
    }
}

/// Byte offsets of every (non-overlapping) occurrence of `needle` in
/// `haystack`. Used by both the legacy replace-all path and the
/// YYC-75 diff scrubber.
fn collect_match_offsets(haystack: &str, needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut start = 0;
    while let Some(rel) = haystack[start..].find(needle) {
        let abs = start + rel;
        out.push(abs);
        start = abs + needle.len();
    }
    out
}

fn apply_accepted_hunks(content: &str, old: &str, new: &str, accepted_offsets: &[usize]) -> String {
    if accepted_offsets.is_empty() {
        return content.to_string();
    }
    let mut out = String::with_capacity(content.len());
    let mut cursor = 0usize;
    let mut sorted = accepted_offsets.to_vec();
    sorted.sort_unstable();
    for offset in sorted {
        if offset < cursor {
            continue;
        }
        out.push_str(&content[cursor..offset]);
        out.push_str(new);
        cursor = offset + old.len();
    }
    out.push_str(&content[cursor..]);
    out
}

async fn run_diff_scrubber(
    tx: &PauseSender,
    path: &str,
    old: &str,
    new: &str,
    content: &str,
    occurrences: &[usize],
) -> Result<Vec<usize>> {
    let hunks: Vec<DiffScrubHunk> = occurrences
        .iter()
        .map(|&offset| DiffScrubHunk {
            offset,
            line_no: line_no_at(content, offset),
            before_lines: split_lines(old),
            after_lines: split_lines(new),
        })
        .collect();
    let (reply_tx, reply_rx) = oneshot::channel();
    let pause = AgentPause {
        kind: PauseKind::DiffScrub {
            path: path.to_string(),
            hunks,
        },
        reply: reply_tx,
        options: Vec::new(),
    };
    if tx.send(pause).await.is_err() {
        // No consumer — fall back to applying every site.
        return Ok(occurrences.to_vec());
    }
    let resume = match tokio::time::timeout(Duration::from_secs(600), reply_rx).await {
        Err(_) => anyhow::bail!("diff scrubber timed out after 10 minutes"),
        Ok(Err(_)) => anyhow::bail!("diff scrubber channel closed"),
        Ok(Ok(r)) => r,
    };
    match resume {
        AgentResume::AcceptHunks(indices) => {
            let mut out: Vec<usize> = indices
                .into_iter()
                .filter_map(|i| occurrences.get(i).copied())
                .collect();
            out.sort_unstable();
            Ok(out)
        }
        AgentResume::Allow | AgentResume::AllowAndRemember => Ok(occurrences.to_vec()),
        AgentResume::Deny | AgentResume::DenyWithReason(_) => Ok(Vec::new()),
        AgentResume::Custom(_) => Ok(Vec::new()),
    }
}

fn line_no_at(content: &str, offset: usize) -> usize {
    let bound = offset.min(content.len());
    1 + content[..bound].bytes().filter(|b| *b == b'\n').count()
}

fn split_lines(s: &str) -> Vec<String> {
    if s.is_empty() {
        return vec![String::new()];
    }
    s.split('\n').map(|l| l.to_string()).collect()
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

        let diff = sink.lock().clone().expect("diff captured");
        assert_eq!(diff.tool, "write_file");
        assert_eq!(diff.path, path_str);
        assert_eq!(diff.before, "old contents");
        assert_eq!(diff.after, "new contents");
    }

    // YYC-159: small files keep working — preflight only blocks
    // pathological sizes.
    #[tokio::test]
    async fn read_file_returns_lines_for_small_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("small.txt");
        std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
        let result = ReadFile
            .call(
                json!({"path": path.to_string_lossy(), "offset": 1, "limit": 10}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("alpha"));
        assert!(result.output.contains("3/3 lines shown"));
    }

    // YYC-199: requesting a small line range from a multi-MiB
    // file should walk only enough of the stream to collect the
    // requested lines + confirm that more exist after. We can't
    // observe peak memory directly in unit tests, but the footer
    // says "more available" without reporting the file's real
    // total — proof that the stream early-exited.
    #[tokio::test]
    async fn read_file_streams_range_without_full_read() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("big.txt");
        // 5_000 lines of "abcdefghij\n" = ~55 KB. Plenty to
        // demonstrate the early-exit behavior.
        let mut contents = String::with_capacity(60_000);
        for i in 0..5_000 {
            contents.push_str(&format!("line {i}\n"));
        }
        std::fs::write(&path, &contents).unwrap();

        let result = ReadFile
            .call(
                json!({"path": path.to_string_lossy(), "offset": 1, "limit": 3}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("line 0"));
        assert!(result.output.contains("line 1"));
        assert!(result.output.contains("line 2"));
        assert!(
            !result.output.contains("line 100"),
            "should not have read past limit",
        );
        assert!(
            result.output.contains("more available"),
            "expected more-available footer, got: {}",
            result.output,
        );
    }

    // YYC-199: when limit covers the entire file, footer reports
    // exact line counts ("X/Y lines shown") and no "more
    // available" hint.
    #[tokio::test]
    async fn read_file_full_range_reports_exact_counts() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("small.txt");
        std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
        let result = ReadFile
            .call(
                json!({"path": path.to_string_lossy(), "offset": 1, "limit": 100}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("3/3 lines shown"));
        assert!(!result.output.contains("more available"));
    }

    // YYC-199: offset past EOF still surfaces the existing
    // structured response.
    #[tokio::test]
    async fn read_file_offset_past_end_returns_message() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tiny.txt");
        std::fs::write(&path, "one\n").unwrap();
        let result = ReadFile
            .call(
                json!({"path": path.to_string_lossy(), "offset": 100, "limit": 10}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.output.contains("offset exceeds"));
    }

    // YYC-159: a file larger than the cap must NOT be loaded into
    // memory. Uses `set_len` to make a sparse file so the test
    // doesn't actually allocate 60 MiB on disk; the OS reports the
    // length, our preflight short-circuits, and `read_to_string` is
    // never reached.
    #[tokio::test]
    async fn read_file_refuses_files_over_cap_without_loading() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("huge.bin");
        let f = std::fs::File::create(&path).unwrap();
        // 60 MiB sparse — above READ_FILE_MAX_BYTES (50 MiB). No data
        // is actually written, so the test does not allocate or read
        // 60 MiB even if the preflight regresses (it would error on
        // invalid UTF-8 instead of OOMing).
        f.set_len(60 * 1024 * 1024).unwrap();
        drop(f);

        let result = ReadFile
            .call(
                json!({"path": path.to_string_lossy(), "offset": 1, "limit": 10}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.is_error, "{}", result.output);
        assert!(
            result.output.contains("exceeds the read_file cap"),
            "expected too-large message, got: {}",
            result.output
        );
        assert!(
            result.output.contains("MiB"),
            "expected size in message: {}",
            result.output
        );
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

        let diff = sink.lock().clone().expect("diff captured");
        assert_eq!(diff.tool, "edit_file");
        assert_eq!(diff.before, "1");
        assert_eq!(diff.after, "42");
    }

    // ── YYC-131: per-call edit_diff travels with ToolResult ─────────────

    #[tokio::test]
    async fn write_file_attaches_per_call_edit_diff_to_tool_result() {
        // Per-call diff lives on ToolResult so AfterToolCall hooks can
        // see *this* call's edit even when the global sink races under
        // concurrent dispatch (YYC-131).
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.txt");
        let path_str = path.to_string_lossy().to_string();

        let sink = crate::tools::new_diff_sink();
        let tool = WriteFile::new(Some(sink.clone()));
        let result = tool
            .call(
                json!({"path": path_str.clone(), "content": "hello"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let diff = result.edit_diff.expect("ToolResult.edit_diff populated");
        assert_eq!(diff.tool, "write_file");
        assert_eq!(diff.path, path_str);
        assert_eq!(diff.after, "hello");
    }

    #[tokio::test]
    async fn patch_file_attaches_per_call_edit_diff_to_tool_result() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.rs");
        let path_str = path.to_string_lossy().to_string();
        std::fs::write(&path, "fn foo() { 1 }\n").unwrap();

        let sink = crate::tools::new_diff_sink();
        let tool = PatchFile::new(Some(sink.clone()));
        let result = tool
            .call(
                json!({"path": path_str.clone(), "old_string": "1", "new_string": "42"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let diff = result.edit_diff.expect("ToolResult.edit_diff populated");
        assert_eq!(diff.tool, "edit_file");
        assert_eq!(diff.path, path_str);
        assert_eq!(diff.before, "1");
        assert_eq!(diff.after, "42");
    }

    #[tokio::test]
    async fn concurrent_writes_to_different_paths_each_carry_their_own_diff() {
        // YYC-131 acceptance pin: concurrent dispatch where two
        // write_file calls hit the global sink near-simultaneously
        // would have produced one ToolResult with the *other* file's
        // diff under the old single-slot scheme. With per-call
        // attachment, each ToolResult carries its own diff regardless
        // of sink ordering.
        let dir = tempdir().unwrap();
        let path_a = dir.path().join("a.txt").to_string_lossy().to_string();
        let path_b = dir.path().join("b.txt").to_string_lossy().to_string();

        let sink = crate::tools::new_diff_sink();
        let tool = std::sync::Arc::new(WriteFile::new(Some(sink.clone())));

        let ta = {
            let tool = tool.clone();
            let path = path_a.clone();
            tokio::spawn(async move {
                tool.call(
                    json!({"path": path, "content": "AAA"}),
                    CancellationToken::new(),
                )
                .await
            })
        };
        let tb = {
            let tool = tool.clone();
            let path = path_b.clone();
            tokio::spawn(async move {
                tool.call(
                    json!({"path": path, "content": "BBB"}),
                    CancellationToken::new(),
                )
                .await
            })
        };
        let (ra, rb) = tokio::join!(ta, tb);
        let ra = ra.unwrap().unwrap();
        let rb = rb.unwrap().unwrap();

        // Each ToolResult carries the diff for its own path — never
        // the other's.
        let da = ra.edit_diff.expect("call A should carry its own edit_diff");
        let db = rb.edit_diff.expect("call B should carry its own edit_diff");
        assert_eq!(da.path, path_a, "call A diff path mismatch");
        assert_eq!(db.path, path_b, "call B diff path mismatch");
        assert_eq!(da.after, "AAA");
        assert_eq!(db.after, "BBB");
    }

    #[test]
    fn collect_match_offsets_finds_every_non_overlapping_site() {
        let content = "alpha beta alpha gamma alpha";
        let offsets = collect_match_offsets(content, "alpha");
        assert_eq!(offsets.len(), 3);
        assert_eq!(offsets[0], 0);
        assert_eq!(&content[offsets[1]..offsets[1] + 5], "alpha");
    }

    #[test]
    fn apply_accepted_hunks_skips_rejected_offsets() {
        let content = "x x x";
        // Only accept the first and third site.
        let offsets = collect_match_offsets(content, "x");
        let accepted = vec![offsets[0], offsets[2]];
        let out = apply_accepted_hunks(content, "x", "Y", &accepted);
        assert_eq!(out, "Y x Y");
    }

    #[test]
    fn apply_accepted_hunks_empty_returns_original() {
        let content = "hello world";
        let out = apply_accepted_hunks(content, "hello", "hi", &[]);
        assert_eq!(out, "hello world");
    }

    #[test]
    fn line_no_at_counts_newlines_before_offset() {
        let content = "line1\nline2\nline3";
        assert_eq!(line_no_at(content, 0), 1);
        assert_eq!(line_no_at(content, 6), 2);
        assert_eq!(line_no_at(content, 12), 3);
    }

    #[tokio::test]
    async fn patch_file_without_pause_replaces_every_occurrence() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("multi.txt");
        let path_str = path.to_string_lossy().to_string();
        std::fs::write(&path, "x x x\n").unwrap();

        let tool = PatchFile::new(None);
        let result = tool
            .call(
                json!({
                    "path": path_str,
                    "old_string": "x",
                    "new_string": "Y",
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, "Y Y Y\n");
    }

    #[tokio::test]
    async fn patch_file_with_pause_routes_through_scrubber_and_applies_subset() {
        use crate::pause;

        let dir = tempdir().unwrap();
        let path = dir.path().join("multi.txt");
        let path_str = path.to_string_lossy().to_string();
        std::fs::write(&path, "x x x\n").unwrap();

        let (tx, mut rx) = pause::channel(2);
        // Simulate a TUI: accept only hunks 0 and 2.
        tokio::spawn(async move {
            if let Some(p) = rx.recv().await {
                let _ = p.reply.send(AgentResume::AcceptHunks(vec![0, 2]));
            }
        });

        let tool = PatchFile::with_pause(None, Some(tx));
        let result = tool
            .call(
                json!({
                    "path": path_str,
                    "old_string": "x",
                    "new_string": "Y",
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, "Y x Y\n");
    }

    #[tokio::test]
    async fn patch_file_with_pause_reject_all_leaves_file_unchanged() {
        use crate::pause;

        let dir = tempdir().unwrap();
        let path = dir.path().join("multi.txt");
        let path_str = path.to_string_lossy().to_string();
        std::fs::write(&path, "x x x\n").unwrap();

        let (tx, mut rx) = pause::channel(2);
        tokio::spawn(async move {
            if let Some(p) = rx.recv().await {
                let _ = p.reply.send(AgentResume::AcceptHunks(Vec::new()));
            }
        });

        let tool = PatchFile::with_pause(None, Some(tx));
        let result = tool
            .call(
                json!({
                    "path": path_str,
                    "old_string": "x",
                    "new_string": "Y",
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, "x x x\n");
    }
}
