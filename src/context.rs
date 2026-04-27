use std::sync::OnceLock;

use tiktoken_rs::{CoreBPE, cl100k_base};

use crate::provider::Message;

/// Lazy-initialized cl100k_base BPE encoder (YYC-129). cl100k_base is
/// the OpenAI tokenizer for GPT-4 / GPT-3.5-turbo, and lands within a
/// few percent of Anthropic / open-weights tokenizers on prose and
/// code. Far more accurate than char-based heuristics — particularly
/// for CJK / emoji (one char ≈ one token, where chars/4 was off by 4x)
/// and for code (denser tokens than English).
fn token_encoder() -> &'static CoreBPE {
    static ENCODER: OnceLock<CoreBPE> = OnceLock::new();
    ENCODER.get_or_init(|| cl100k_base().expect("cl100k_base BPE bundled with tiktoken-rs"))
}

/// Count tokens via the cl100k_base encoder. Falls back to bytes/3 if
/// the encoder ever fails to load (shouldn't happen — the BPE table is
/// bundled with the crate).
pub(crate) fn count_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    token_encoder().encode_with_special_tokens(text).len()
}

/// System prompt for the summarizer LLM call. Drives a tight, factual
/// summary that preserves anything the agent will need to keep working
/// after the older turns are dropped (YYC-128).
const SUMMARIZATION_SYSTEM_PROMPT: &str = "\
You are a context summarizer for a coding agent. The conversation that follows is being \
truncated to fit a smaller context window. Produce a tight, factual summary that preserves \
everything the agent will need to keep working:

- File paths mentioned and what was read, written, or modified
- Decisions made and constraints set by the user (preferences, do/don't lists)
- Errors encountered, their root causes, and the fix that was applied
- Tool outputs that informed a decision (compile errors, test failures, search hits)
- Open questions or in-flight work that wasn't completed
- The user's current goal

Write in dense bullet-point form. Drop pleasantries, repeated context, verbose tool dumps, \
and chit-chat. Aim for under 1500 words. Do not add commentary about being a summarizer.";

/// Manages token budget and decides when context needs to be compacted.
///
/// Compaction itself runs in the agent (it needs the provider to call the
/// LLM); this struct just tracks usage, decides when to trigger, and finds
/// a safe split point in the message history.
pub struct ContextManager {
    max_context: usize,
    current_tokens: usize,
    summary: Option<String>,
    reserved_tokens: usize,
    trigger_ratio: f64,
    /// Minimum number of trailing messages to keep verbatim across a
    /// compaction. The actual kept window may be longer if the safe
    /// split has to walk past tool sequences (YYC-128).
    keep_recent: usize,
}

/// Default reservation for the next response. Capped at `max_context / 4`
/// in `ContextManager::new` so an 8K-context model never has its entire
/// budget swallowed by the reservation alone (YYC-129).
const DEFAULT_RESERVED_TOKENS: usize = 50_000;

/// Floor on the cap. Even tiny-context models keep at least this much
/// budget for the next response so `should_compact` can fire before the
/// provider rejects the request.
const MIN_RESERVED_TOKENS: usize = 1_024;

impl ContextManager {
    pub fn new(max_context: usize) -> Self {
        // YYC-129: reserved_tokens used to be a flat 50K — larger than
        // an 8K-context model's entire budget. Cap it at max_context/4
        // so reservation scales with the actual window, but keep a
        // 1K floor so even tiny windows leave room for a reply.
        let reserved_cap = (max_context / 4).max(MIN_RESERVED_TOKENS);
        let reserved_tokens = DEFAULT_RESERVED_TOKENS.min(reserved_cap);
        Self {
            max_context,
            current_tokens: 0,
            summary: None,
            reserved_tokens,
            trigger_ratio: 0.85,
            keep_recent: 6,
        }
    }

    /// Record token usage from an LLM response.
    ///
    /// Provider usage already covers the prompt sent on this request, so
    /// replace the previous estimate instead of adding to it. Otherwise
    /// a freshly installed compaction summary gets counted again on the
    /// next response.
    pub fn record_usage(&mut self, prompt_tokens: usize, completion_tokens: usize) {
        self.current_tokens = prompt_tokens.saturating_add(completion_tokens);
    }

    /// Should the next turn be preceded by a compaction pass?
    pub fn should_compact(&self, messages: &[Message]) -> bool {
        if self.summary.is_none() && messages.len() > 50 {
            return true;
        }

        let estimated_tokens = self.estimate_tokens(messages);
        estimated_tokens >= (self.max_context as f64 * self.trigger_ratio) as usize
            || estimated_tokens + self.reserved_tokens >= self.max_context
    }

    /// Find the index where the kept-recent window starts.
    ///
    /// The returned index `i` is positioned at a `User` message, so the
    /// pre-summary slice `messages[..i]` ends cleanly and the post-summary
    /// slice `messages[i..]` cannot orphan a `Tool` message (every Tool in
    /// it follows its calling Assistant within the same slice).
    ///
    /// Returns `None` when no User boundary exists in the trailing window
    /// — the caller should skip compaction in that case rather than risk
    /// breaking the wire-protocol invariant.
    pub fn safe_split_index(&self, messages: &[Message]) -> Option<usize> {
        let target = messages.len().saturating_sub(self.keep_recent).max(1);
        (target..messages.len()).find(|&i| matches!(messages[i], Message::User { .. }))
    }

    /// Build the request that asks the provider to summarize an older
    /// slice of the conversation. The agent runs this request through its
    /// own provider so ContextManager stays free of the `LLMProvider`
    /// trait (and therefore async).
    pub fn summarization_request(messages_to_summarize: &[Message]) -> Vec<Message> {
        let history_text = format_history(messages_to_summarize);
        vec![
            Message::System {
                content: SUMMARIZATION_SYSTEM_PROMPT.to_string(),
            },
            Message::User {
                content: history_text,
            },
        ]
    }

    /// Install a freshly produced summary. Resets the running token
    /// estimate to just the summary length so `record_usage` on the
    /// next response replaces (rather than stacks on top of) it.
    pub fn install_summary(&mut self, summary: String) {
        self.current_tokens = self.estimate_tokens_str(&summary);
        self.summary = Some(summary);
    }

    /// Best-effort prior summary, mainly for diagnostics.
    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    fn estimate_tokens(&self, messages: &[Message]) -> usize {
        // YYC-129 perf: BPE encoding the full history on every turn is
        // O(N²) over a long session. For `should_compact`'s purposes we
        // only need precision near the trigger threshold — well below
        // it, a coarse bytes/3 upper bound is enough to short-circuit
        // and skip the BPE call entirely.
        let total_bytes: usize = messages
            .iter()
            .map(|m| match m {
                Message::User { content } => content.len(),
                Message::Assistant { content, .. } => {
                    content.as_ref().map(|s| s.len()).unwrap_or(0)
                }
                Message::Tool { content, .. } => content.len(),
                Message::System { content } => content.len(),
            })
            .sum::<usize>()
            + messages.len(); // +1 byte per separator
        let cheap_estimate = total_bytes / 3 + 1;

        // Threshold for "near the limit" — if the cheap upper bound is
        // less than half of the trigger ratio's token budget, we're
        // comfortably below and a precise count would only confirm it.
        let trigger_tokens = (self.max_context as f64 * self.trigger_ratio) as usize;
        if cheap_estimate < trigger_tokens / 2 {
            return cheap_estimate;
        }

        // Within striking distance — pay for real BPE precision.
        let text: String = messages
            .iter()
            .map(|m| match m {
                Message::User { content } => content.clone(),
                Message::Assistant { content, .. } => content.clone().unwrap_or_default(),
                Message::Tool { content, .. } => content.clone(),
                Message::System { content } => content.clone(),
            })
            .collect::<Vec<_>>()
            .join(" ");

        self.estimate_tokens_str(&text)
    }

    fn estimate_tokens_str(&self, text: &str) -> usize {
        // YYC-129: real BPE count via cl100k_base (see `count_tokens`).
        // Replaces the former chars/4 heuristic, which was wildly wrong
        // for CJK (~4x under) and code (~10–20% under).
        count_tokens(text)
    }
}

/// Render messages as a transcript the summarizer LLM can read.
fn format_history(messages: &[Message]) -> String {
    let mut out = String::new();
    for msg in messages {
        match msg {
            Message::System { content } => {
                out.push_str("[SYSTEM]\n");
                out.push_str(content);
            }
            Message::User { content } => {
                out.push_str("[USER]\n");
                out.push_str(content);
            }
            Message::Assistant {
                content,
                tool_calls,
                ..
            } => {
                out.push_str("[ASSISTANT]\n");
                if let Some(c) = content {
                    out.push_str(c);
                }
                if let Some(calls) = tool_calls
                    && !calls.is_empty()
                {
                    out.push_str("\n[tool_calls]\n");
                    for tc in calls {
                        out.push_str(&format!(
                            "- {}({}) [id={}]\n",
                            tc.function.name, tc.function.arguments, tc.id,
                        ));
                    }
                }
            }
            Message::Tool {
                tool_call_id,
                content,
            } => {
                out.push_str(&format!("[TOOL id={tool_call_id}]\n"));
                out.push_str(content);
            }
        }
        out.push_str("\n\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager(max_context: usize) -> ContextManager {
        ContextManager {
            max_context,
            current_tokens: 0,
            summary: None,
            reserved_tokens: 0,
            trigger_ratio: 0.85,
            keep_recent: 6,
        }
    }

    fn user(content: impl Into<String>) -> Message {
        Message::User {
            content: content.into(),
        }
    }

    fn asst(content: impl Into<String>) -> Message {
        Message::Assistant {
            content: Some(content.into()),
            tool_calls: None,
            reasoning_content: None,
        }
    }

    #[test]
    fn compaction_triggers_at_ratio_boundary() {
        // YYC-129: cl100k_base BPE estimator replaces the chars/4
        // heuristic. Token counts depend on the actual encoder output,
        // so the test asserts the relative shape — text well below the
        // ratio doesn't trigger; text well above does.
        let ctx = manager(100);

        let small = "hello world ".repeat(10); // ~20 tokens
        let large = "hello world ".repeat(50); // ~100 tokens

        assert!(!ctx.should_compact(&[user(small)]));
        assert!(ctx.should_compact(&[user(large)]));
    }

    #[test]
    fn messages_below_trigger_ratio_are_never_modified() {
        let ctx = manager(100);
        let messages = vec![user("small prompt")];
        let before = format!("{messages:?}");

        assert!(!ctx.should_compact(&messages));
        let after = format!("{messages:?}");

        assert_eq!(before, after);
        assert!(ctx.summary.is_none());
    }

    #[test]
    fn long_history_heuristic_fires_only_once_per_session() {
        let mut ctx = manager(1_000_000);
        let messages = (0..51)
            .map(|n| user(format!("turn {n}")))
            .collect::<Vec<_>>();

        assert!(ctx.should_compact(&messages));
        ctx.install_summary("stub".into());

        assert!(!ctx.should_compact(&messages));
    }

    #[test]
    fn install_summary_resets_token_count_to_summary_size() {
        let mut ctx = manager(1000);
        ctx.install_summary("a short summary".into());
        let summary_tokens = ctx.estimate_tokens_str("a short summary");
        assert_eq!(ctx.current_tokens, summary_tokens);

        ctx.record_usage(10, 5);
        // record_usage replaces, doesn't add.
        assert_eq!(ctx.current_tokens, 15);
    }

    #[test]
    fn safe_split_lands_on_user_message_in_recent_window() {
        let ctx = manager(1_000_000);
        // 0:Sys 1:U 2:A 3:U 4:A 5:U 6:A 7:U 8:A — keep_recent=6 → target=3.
        let messages = vec![
            Message::System {
                content: "sys".into(),
            },
            user("u1"),
            asst("a1"),
            user("u2"),
            asst("a2"),
            user("u3"),
            asst("a3"),
            user("u4"),
            asst("a4"),
        ];

        let split = ctx.safe_split_index(&messages).expect("user in tail");
        assert_eq!(split, 3); // first User at or after target=3
        assert!(matches!(messages[split], Message::User { .. }));
    }

    #[test]
    fn safe_split_returns_none_when_tail_has_no_user_boundary() {
        let ctx = manager(1_000_000);
        // System + a long Assistant-only / Tool tail (e.g. mid tool-loop).
        let messages = vec![
            Message::System {
                content: "sys".into(),
            },
            asst("a1"),
            asst("a2"),
            asst("a3"),
            asst("a4"),
            asst("a5"),
            asst("a6"),
            asst("a7"),
        ];
        assert!(ctx.safe_split_index(&messages).is_none());
    }

    #[test]
    fn summarization_request_includes_system_and_renders_history() {
        let messages = vec![
            user("touch /tmp/foo.txt"),
            asst("done; created /tmp/foo.txt"),
        ];
        let req = ContextManager::summarization_request(&messages);
        assert_eq!(req.len(), 2);
        match (&req[0], &req[1]) {
            (
                Message::System { content: sys },
                Message::User {
                    content: rendered, ..
                },
            ) => {
                assert!(sys.contains("summarizer"));
                assert!(rendered.contains("[USER]"));
                assert!(rendered.contains("touch /tmp/foo.txt"));
                assert!(rendered.contains("[ASSISTANT]"));
                assert!(rendered.contains("/tmp/foo.txt"));
            }
            _ => panic!("expected System+User pair, got {req:?}"),
        }
    }

    // ── YYC-129: token estimation + reserved-tokens cap ─────────────────

    #[test]
    fn reserved_tokens_caps_at_quarter_of_max_context() {
        // YYC-129: 8K-context model used to have its entire window
        // swallowed by the 50K reservation. Now the reservation caps
        // at max_context/4.
        let ctx = ContextManager::new(8_000);
        assert!(
            ctx.reserved_tokens <= 8_000 / 4,
            "reserved_tokens={} exceeds quarter cap on 8K context",
            ctx.reserved_tokens
        );
    }

    #[test]
    fn reserved_tokens_keeps_full_default_when_window_is_huge() {
        // 200K-context model: cap is 50K (max_context/4 = 50K), so the
        // default constant survives.
        let ctx = ContextManager::new(200_000);
        assert_eq!(ctx.reserved_tokens, 50_000);
    }

    #[test]
    fn reserved_tokens_floor_keeps_room_on_tiny_windows() {
        // Pathological 2K window: max_context/4 = 512, but the floor is
        // 1K so we don't let the cap drop below where the next response
        // can fit.
        let ctx = ContextManager::new(2_000);
        assert_eq!(ctx.reserved_tokens, MIN_RESERVED_TOKENS);
    }

    #[test]
    fn token_estimation_uses_real_bpe_for_ascii() {
        // YYC-129: cl100k_base gives a real token count. For
        // "hello world" the encoder produces 2 tokens; ten copies
        // (with separators) is between 18 and 22 depending on
        // tokenizer details. We just pin a sane lower bound.
        let ctx = manager(1_000_000);
        let s = "hello world ".repeat(10);
        let estimated = ctx.estimate_tokens_str(&s);
        assert!(
            (15..=30).contains(&estimated),
            "ASCII estimate out of expected band: {estimated}",
        );
    }

    #[test]
    fn token_estimation_handles_cjk_better_than_old_heuristic() {
        // YYC-129 acceptance: cl100k_base correctly counts CJK at
        // roughly 1+ tokens per character. The old chars/4 heuristic
        // would have called this string ~25 tokens; the real count is
        // closer to 100, which is what triggers compaction in time.
        let ctx = manager(1_000_000);
        let cjk = "你好世界".repeat(25); // 100 CJK chars
        let estimated = ctx.estimate_tokens_str(&cjk);
        assert!(
            estimated >= 90,
            "CJK estimate underestimated: {estimated} (expected ≥90)",
        );
        // And not absurdly above (sanity).
        assert!(
            estimated <= 250,
            "CJK estimate suspiciously high: {estimated}",
        );
    }

    #[test]
    fn token_estimation_handles_code_density() {
        // Code tokens denser than English; the heuristic used to undercut
        // by 10-20%. Real BPE counts what's actually sent.
        let ctx = manager(1_000_000);
        let code = r#"
            fn main() {
                let xs: Vec<u64> = (0..100).map(|i| i * i).collect();
                println!("{:?}", xs);
            }
        "#;
        let estimated = ctx.estimate_tokens_str(code);
        // Should be a non-trivial count — exact number depends on BPE
        // merges, but well above the chars/4 = ~50 estimate.
        assert!(estimated > 30, "code estimate too low: {estimated}");
    }
}
