//! AfterToolCall hook that auto-stores notable tool outputs into the Cortex
//! knowledge graph (Phase 2, YYC-264).
//!
//! On every tool call that returns meaningful content, the tool name + a
//! condensed summary of the result are stored as a `fact` node in the graph.
//! The hook uses a simple time-windowed bloom filter to avoid storing near-
//! identical consecutive entries for the same tool, keeping the graph clean.
//!
//! Graceful degradation: errors are logged and skipped — never breaks the
//! agent loop. Falls through silently when cortex is disabled or the tool
//! is on the skip list.

use anyhow::Result;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::memory::cortex::CortexStore;
use crate::tools::ToolResult;

use super::{HookHandler, HookOutcome};

/// Tools whose outputs are too noisy or trivial to store.
const SKIP_TOOLS: &[&str] = &["list_sessions", "list_messages", "list_files", "get_prompt"];

/// Tools whose outputs tend to be large or ephemeral.
const TRANSIENT_TOOLS: &[&str] = &["bash", "terminal", "think"];

/// Max chars of tool output to store in a single node.
const MAX_BODY_LENGTH: usize = 500;

/// How many recent captures to remember for dedup (per tool).
const DEDUP_WINDOW: usize = 5;

/// Per-tool dedup ring buffer: the last N hashes of stored content.
struct DedupRing {
    by_tool: Mutex<std::collections::HashMap<String, VecDeque<u64>>>,
}

impl DedupRing {
    fn new() -> Self {
        Self {
            by_tool: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Returns `true` if the given content hash is a duplicate of a recent
    /// entry for the same tool.
    fn is_duplicate(&self, tool: &str, hash: u64) -> bool {
        let mut map = self.by_tool.lock().unwrap();
        let ring = map
            .entry(tool.to_string())
            .or_insert_with(|| VecDeque::with_capacity(DEDUP_WINDOW + 1));
        if ring.contains(&hash) {
            return true;
        }
        ring.push_back(hash);
        if ring.len() > DEDUP_WINDOW {
            ring.pop_front();
        }
        false
    }
}

pub struct CortexCaptureHook {
    store: Arc<CortexStore>,
    dedup: DedupRing,
}

impl CortexCaptureHook {
    pub fn new(store: Arc<CortexStore>) -> Self {
        Self {
            store,
            dedup: DedupRing::new(),
        }
    }

    /// Quick hash of the tool output for dedup purposes.
    fn content_hash(tool: &str, output: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        tool.hash(&mut hasher);
        // Only hash first 200 chars to catch near-identical repeats
        let truncated: &str = if output.len() > 200 {
            &output[..200]
        } else {
            output
        };
        truncated.hash(&mut hasher);
        hasher.finish()
    }

    /// Condense a tool output into a short summary suitable for a graph node body.
    fn summarize(output: &str, is_error: bool) -> String {
        if is_error {
            let preview: String = output.chars().take(200).collect();
            return format!("Error: {}", preview.replace('\n', " "));
        }
        let stripped: String = output
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if stripped.len() <= MAX_BODY_LENGTH {
            stripped
        } else {
            format!("{}...", &stripped[..MAX_BODY_LENGTH - 3])
        }
    }
}

#[async_trait]
impl HookHandler for CortexCaptureHook {
    fn name(&self) -> &str {
        "cortex-capture"
    }

    /// Run after all other hooks so we see the final tool result.
    fn priority(&self) -> i32 {
        30
    }

    async fn after_tool_call(
        &self,
        tool: &str,
        result: &ToolResult,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        // Skip noisy tools.
        if SKIP_TOOLS.contains(&tool) {
            return Ok(HookOutcome::Continue);
        }

        let output = result.output.trim();
        if output.is_empty() {
            return Ok(HookOutcome::Continue);
        }

        // Dedup: don't store near-identical consecutive entries.
        let hash = Self::content_hash(tool, output);
        if self.dedup.is_duplicate(tool, hash) {
            return Ok(HookOutcome::Continue);
        }

        let summary = Self::summarize(output, result.is_error);
        let importance = if result.is_error { 0.6 } else { 0.4 };

        // Build a descriptive title from the tool name and a preview.
        let preview: String = output.chars().take(60).collect();
        let preview = preview.replace('\n', " ").trim().to_string();
        let title = format!("[{}] {}", tool, preview);

        // For transient tools (bash, terminal), prefix with "tool:" so they
        // are distinguishable from explicit facts.
        let node = if TRANSIENT_TOOLS.contains(&tool) {
            CortexStore::fact(&title, importance)
        } else {
            let node = CortexStore::fact(&title, importance);
            node
        };

        match self.store.store(node) {
            Ok(_id) => {
                tracing::trace!(
                    "cortex-capture: stored {tool} result ({})",
                    summary.chars().take(40).collect::<String>()
                );
            }
            Err(e) => {
                tracing::warn!("cortex-capture: failed to store {tool} result: {e}");
            }
        }

        Ok(HookOutcome::Continue)
    }
}
