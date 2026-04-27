//! Wire-debug log redaction (YYC-135).
//!
//! `do_request` and `log_wire_response` log raw request/response bodies at
//! `info!` when `debug = "wire"`. Those bodies routinely include:
//!
//! - Conversation history (often confidential)
//! - Tool outputs that read config files (`api_key` material)
//! - File contents the agent was asked to inspect
//! - The `Bearer <token>` header (only in `Authorization`, but error paths
//!   echo the request shape)
//!
//! Without redaction, `debug = "wire"` silently turns the local log file
//! into a secret-exfiltration vector — exactly the file a user is most
//! likely to copy into a support ticket.
//!
//! This module exposes:
//! - `redact_string`: strip recognizable secret shapes and truncate long
//!   strings (preserving the head so the log is still useful).
//! - `redact_value`: deep walk a `serde_json::Value`, applying
//!   `redact_string` to every string leaf.
//! - `redact_response_text`: same but for free-form raw response bodies
//!   (which may be SSE text rather than JSON).

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

/// Strings longer than this are truncated to head + elision marker. The
/// log still surfaces the first chunk (enough to debug shape / leading
/// fields) without dumping kilobytes of file content per request.
pub(crate) const MAX_STRING_LEN: usize = 500;

/// Compiled secret-shape patterns. Lazy-initialized once per process.
fn secret_patterns() -> &'static [(Regex, &'static str)] {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            // `Bearer <token>` — preserve the label, redact the token.
            // Captures group 1 = the prefix so callers can splice it back.
            (
                Regex::new(r"(?i)(Bearer\s+)[A-Za-z0-9_\-\.=/+]{8,}").expect("Bearer regex"),
                "$1[REDACTED]",
            ),
            // OpenAI / Anthropic / OpenRouter family.
            (
                Regex::new(r"sk-ant-[A-Za-z0-9_\-]{16,}").expect("anthropic key regex"),
                "sk-ant-[REDACTED]",
            ),
            (
                Regex::new(r"sk-or-[A-Za-z0-9_\-]{16,}").expect("openrouter key regex"),
                "sk-or-[REDACTED]",
            ),
            (
                Regex::new(r"sk-[A-Za-z0-9_\-]{16,}").expect("sk- key regex"),
                "sk-[REDACTED]",
            ),
            // GitHub personal access tokens / fine-grained tokens.
            (
                Regex::new(r"ghp_[A-Za-z0-9]{20,}").expect("github pat regex"),
                "ghp_[REDACTED]",
            ),
            (
                Regex::new(r"github_pat_[A-Za-z0-9_]{20,}").expect("github fine-grained regex"),
                "github_pat_[REDACTED]",
            ),
            // AWS access key ID shape.
            (
                Regex::new(r"AKIA[0-9A-Z]{16}").expect("aws key id regex"),
                "AKIA[REDACTED]",
            ),
        ]
    })
}

/// Strip recognizable secret shapes from `s`, then truncate if longer than
/// `MAX_STRING_LEN` chars. Designed to be safe to call on any string that
/// might land in a debug log.
pub fn redact_string(s: &str) -> String {
    let mut out = s.to_string();
    for (re, replacement) in secret_patterns() {
        let replaced = re.replace_all(&out, *replacement);
        out = replaced.into_owned();
    }
    if out.chars().count() > MAX_STRING_LEN {
        let elided = out.chars().count() - MAX_STRING_LEN;
        let head: String = out.chars().take(MAX_STRING_LEN).collect();
        out = format!("{head}… [{elided} chars elided]");
    }
    out
}

/// Deep-walk a JSON value and apply `redact_string` to every string leaf.
/// Object keys are kept as-is (they're schema, not secret data).
pub fn redact_value(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(redact_string(s)),
        Value::Array(arr) => Value::Array(arr.iter().map(redact_value).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), redact_value(v));
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}

/// Redact a free-form (non-JSON) response body. SSE streams are line-based
/// text — apply `redact_string` then re-truncate the whole blob so a
/// chunked response can't expand past the cap.
pub fn redact_response_text(text: &str) -> String {
    redact_string(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redact_string_strips_bearer_token() {
        let raw = "Authorization: Bearer sk-test-1234567890abcdef";
        let red = redact_string(raw);
        assert!(!red.contains("sk-test-1234567890abcdef"));
        assert!(red.contains("Bearer [REDACTED]"));
    }

    #[test]
    fn redact_string_strips_bare_sk_key() {
        let raw = "config api_key = \"sk-1234567890abcdef0123\"";
        let red = redact_string(raw);
        assert!(!red.contains("sk-1234567890abcdef0123"));
        assert!(red.contains("sk-[REDACTED]"));
    }

    #[test]
    fn redact_string_strips_anthropic_key() {
        let raw = "key=sk-ant-api03-AAAAAAAAAAAAAAAAA";
        let red = redact_string(raw);
        assert!(!red.contains("sk-ant-api03-AAAAAAAAAAAAAAAAA"));
        assert!(red.contains("sk-ant-[REDACTED]"));
    }

    #[test]
    fn redact_string_strips_openrouter_key() {
        let raw = "OPENROUTER_API_KEY=sk-or-v1-abcdefghijklmnopqrstuvwx";
        let red = redact_string(raw);
        assert!(!red.contains("sk-or-v1-abcdefghijklmnopqrstuvwx"));
        assert!(red.contains("sk-or-[REDACTED]"));
    }

    #[test]
    fn redact_string_strips_github_token() {
        let raw = "ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ012345";
        let red = redact_string(raw);
        assert!(!red.contains("aBcDeFgHiJkLmNoPqRsTuVwXyZ012345"));
        assert!(red.contains("ghp_[REDACTED]"));
    }

    #[test]
    fn redact_string_strips_aws_access_key_id() {
        let raw = "AKIAIOSFODNN7EXAMPLE";
        let red = redact_string(raw);
        assert!(red.contains("AKIA[REDACTED]"));
    }

    #[test]
    fn redact_string_truncates_long_payload() {
        let raw = "a".repeat(MAX_STRING_LEN + 200);
        let red = redact_string(&raw);
        assert!(red.contains("[200 chars elided]"));
        assert!(red.chars().count() < MAX_STRING_LEN + 100);
    }

    #[test]
    fn redact_string_short_passthrough_unchanged() {
        let raw = "the quick brown fox";
        assert_eq!(redact_string(raw), raw);
    }

    #[test]
    fn redact_value_walks_nested_objects_and_arrays() {
        let v = json!({
            "model": "claude-haiku-4.5",
            "messages": [
                {"role": "system", "content": "You are an assistant."},
                {"role": "user", "content": "use Bearer sk-test-aaaaaaaaaaaaaaaaaaaa"},
            ],
            "tools": [{"name": "bash", "args": {"command": "echo sk-1234567890abcdef0123"}}],
        });

        let red = redact_value(&v);
        let serialized = red.to_string();
        assert!(!serialized.contains("sk-test-aaaaaaaaaaaaaaaaaaaa"));
        assert!(!serialized.contains("sk-1234567890abcdef0123"));
        assert!(serialized.contains("Bearer [REDACTED]"));
        assert!(serialized.contains("sk-[REDACTED]"));
        // Schema keys are preserved verbatim — they're not secret data.
        assert!(serialized.contains("messages"));
        assert!(serialized.contains("tools"));
        // Non-secret content survives.
        assert!(serialized.contains("You are an assistant."));
    }

    #[test]
    fn redact_value_truncates_long_string_leaves() {
        let big = "x".repeat(MAX_STRING_LEN + 50);
        let v = json!({"messages": [{"role": "user", "content": big}]});
        let red = redact_value(&v);
        let s = red.to_string();
        assert!(s.contains("[50 chars elided]"));
    }

    #[test]
    fn redact_response_text_handles_sse_with_embedded_keys() {
        let raw = "data: {\"id\":\"x\",\"choices\":[{\"delta\":{\"content\":\"see Bearer sk-leak-123456789012345678\"}}]}\n";
        let red = redact_response_text(raw);
        assert!(!red.contains("sk-leak-123456789012345678"));
        assert!(red.contains("Bearer [REDACTED]"));
    }
}
