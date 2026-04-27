//! `<think>…</think>` extractor for streaming providers (YYC-103).
//!
//! Local / open-weight models (llama.cpp, Qwen reasoning variants,
//! DeepSeek-R1 distills) emit chain-of-thought as inline `<think>` tags
//! in the regular content stream rather than the structured
//! `reasoning_content` field. This sanitizer is a small state machine
//! that splits each text chunk into visible content + reasoning trace
//! across chunk boundaries (no tag is required to land in a single
//! delta).
//!
//! Tag matching is case-insensitive on the tag name; attributes are
//! tolerated (`<think id="…">`). The sanitizer never swallows real
//! text — anything that looks like a partial tag but doesn't resolve
//! is flushed as plain content on the next chunk that disambiguates it.

#[derive(Debug, Default)]
pub struct ThinkSanitizer {
    in_think: bool,
    /// Buffered tail that begins with `<` and might still resolve to a
    /// `<think...>` or `</think...>` tag. Stays empty in the steady
    /// state.
    pending: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SanitizeOut {
    pub text: String,
    pub reasoning: String,
}

const OPEN_TAG: &str = "<think";
const CLOSE_TAG: &str = "</think";

impl ThinkSanitizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one streamed chunk. Returns the visible text + reasoning
    /// fragments to emit. Pending partial tags are buffered until a
    /// future chunk resolves them.
    pub fn feed(&mut self, chunk: &str) -> SanitizeOut {
        let mut input = std::mem::take(&mut self.pending);
        input.push_str(chunk);
        let mut out = SanitizeOut::default();
        let mut cursor = 0usize;
        while cursor < input.len() {
            let rest = &input[cursor..];
            if self.in_think {
                // Look for the closing tag.
                match find_tag_ci(rest, CLOSE_TAG) {
                    TagMatch::Full { start, end } => {
                        out.reasoning.push_str(&rest[..start]);
                        cursor += end;
                        self.in_think = false;
                    }
                    TagMatch::Partial { start } => {
                        out.reasoning.push_str(&rest[..start]);
                        self.pending.push_str(&rest[start..]);
                        return out;
                    }
                    TagMatch::None => {
                        out.reasoning.push_str(rest);
                        return out;
                    }
                }
            } else {
                match find_tag_ci(rest, OPEN_TAG) {
                    TagMatch::Full { start, end } => {
                        out.text.push_str(&rest[..start]);
                        cursor += end;
                        self.in_think = true;
                    }
                    TagMatch::Partial { start } => {
                        out.text.push_str(&rest[..start]);
                        self.pending.push_str(&rest[start..]);
                        return out;
                    }
                    TagMatch::None => {
                        out.text.push_str(rest);
                        return out;
                    }
                }
            }
        }
        out
    }

    /// Flush any pending buffer. Called once after the upstream stream
    /// closes so partial tags that never resolved are still emitted.
    pub fn flush(&mut self) -> SanitizeOut {
        let pending = std::mem::take(&mut self.pending);
        let mut out = SanitizeOut::default();
        if pending.is_empty() {
            return out;
        }
        if self.in_think {
            out.reasoning.push_str(&pending);
        } else {
            out.text.push_str(&pending);
        }
        out
    }
}

#[derive(Debug)]
enum TagMatch {
    /// Full tag consumed. `start` = byte offset of `<`. `end` = byte
    /// offset just past `>`.
    Full {
        start: usize,
        end: usize,
    },
    /// A `<` was found but the rest of the buffer is too short to
    /// disambiguate. `start` = byte offset of the `<`.
    Partial {
        start: usize,
    },
    None,
}

fn find_tag_ci(haystack: &str, prefix: &str) -> TagMatch {
    let prefix_lower = prefix.to_ascii_lowercase();
    let prefix_bytes = prefix_lower.as_bytes();
    let bytes = haystack.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let avail = bytes.len() - i;
        // Compare character-by-character against prefix (case-insensitive
        // for ASCII letters).
        let mut matched = 0usize;
        while matched < prefix_bytes.len() && matched < avail {
            let a = bytes[i + matched].to_ascii_lowercase();
            let b = prefix_bytes[matched];
            if a != b {
                break;
            }
            matched += 1;
        }
        if matched < prefix_bytes.len() {
            if matched == avail && matched > 0 {
                // Buffer ended mid-prefix — the next chunk could still
                // complete the tag.
                return TagMatch::Partial { start: i };
            }
            i += 1;
            continue;
        }
        // Prefix matched. We now need to find the closing `>` for the
        // open tag (allowing whitespace/attributes).
        let after = i + matched;
        let mut j = after;
        while j < bytes.len() && bytes[j] != b'>' {
            j += 1;
        }
        if j == bytes.len() {
            // Tag opened but no closing `>` yet — buffer for the next
            // chunk.
            return TagMatch::Partial { start: i };
        }
        // Validate the char immediately after the prefix is either
        // `>` or whitespace (rejects `<thinker>` etc.).
        let after_byte = bytes[after];
        let prefix_terminates = after_byte == b'>'
            || after_byte == b' '
            || after_byte == b'\t'
            || after_byte == b'\n'
            || after_byte == b'/';
        if !prefix_terminates {
            i += 1;
            continue;
        }
        return TagMatch::Full {
            start: i,
            end: j + 1,
        };
    }
    TagMatch::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_block_in_one_chunk_routes_to_reasoning() {
        let mut s = ThinkSanitizer::new();
        let out = s.feed("Hello <think>secret thoughts</think> world");
        assert_eq!(out.text, "Hello  world");
        assert_eq!(out.reasoning, "secret thoughts");
    }

    #[test]
    fn block_split_across_two_chunks() {
        let mut s = ThinkSanitizer::new();
        let a = s.feed("Hello <think>part1");
        assert_eq!(a.text, "Hello ");
        assert_eq!(a.reasoning, "part1");
        let b = s.feed(" part2</think> world");
        assert_eq!(b.reasoning, " part2");
        assert_eq!(b.text, " world");
    }

    #[test]
    fn opening_tag_split_across_chunks() {
        let mut s = ThinkSanitizer::new();
        let a = s.feed("Hello <thi");
        assert_eq!(a.text, "Hello ");
        assert!(a.reasoning.is_empty());
        let b = s.feed("nk>secret</think>!");
        assert_eq!(b.text, "!");
        assert_eq!(b.reasoning, "secret");
    }

    #[test]
    fn case_insensitive_tag_match() {
        let mut s = ThinkSanitizer::new();
        let out = s.feed("Pre <Think>x</THINK> post");
        assert_eq!(out.text, "Pre  post");
        assert_eq!(out.reasoning, "x");
    }

    #[test]
    fn partial_open_with_attribute_then_close() {
        let mut s = ThinkSanitizer::new();
        let out = s.feed("Pre <think id=\"a\">trace</think> post");
        assert_eq!(out.text, "Pre  post");
        assert_eq!(out.reasoning, "trace");
    }

    #[test]
    fn lone_lt_passes_through() {
        let mut s = ThinkSanitizer::new();
        let out = s.feed("a < b");
        assert_eq!(out.text, "a < b");
    }

    #[test]
    fn fake_tag_passes_through() {
        let mut s = ThinkSanitizer::new();
        // <thinker> is not a think tag.
        let out = s.feed("ok <thinker> wat");
        assert_eq!(out.text, "ok <thinker> wat");
    }

    #[test]
    fn multiple_blocks_per_turn() {
        let mut s = ThinkSanitizer::new();
        let out = s.feed("a<think>1</think>b<think>2</think>c");
        assert_eq!(out.text, "abc");
        assert_eq!(out.reasoning, "12");
    }

    #[test]
    fn flush_drains_pending_as_plain_text() {
        let mut s = ThinkSanitizer::new();
        let _ = s.feed("foo <thi");
        let out = s.flush();
        assert_eq!(out.text, "<thi");
    }
}
