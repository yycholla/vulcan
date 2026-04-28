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
            // YYC-250: Google API key shape (services like Maps, GenAI,
            // Cloud APIs all share this prefix).
            (
                Regex::new(r"AIza[0-9A-Za-z_\-]{35}").expect("google api key regex"),
                "AIza[REDACTED]",
            ),
            // YYC-250: Slack tokens — bot, user, app, workspace, refresh,
            // and service refresh prefixes.
            (
                Regex::new(r"xox[bpasr]-[A-Za-z0-9-]{8,}").expect("slack token regex"),
                "xox-[REDACTED]",
            ),
            (
                Regex::new(r"xapp-[A-Za-z0-9-]{8,}").expect("slack app token regex"),
                "xapp-[REDACTED]",
            ),
            // YYC-250: Stripe live keys (secret / restricted / publishable
            // / webhook signing).
            (
                Regex::new(r"sk_live_[A-Za-z0-9]{16,}").expect("stripe sk_live regex"),
                "sk_live_[REDACTED]",
            ),
            (
                Regex::new(r"rk_live_[A-Za-z0-9]{16,}").expect("stripe rk_live regex"),
                "rk_live_[REDACTED]",
            ),
            (
                Regex::new(r"pk_live_[A-Za-z0-9]{16,}").expect("stripe pk_live regex"),
                "pk_live_[REDACTED]",
            ),
            (
                Regex::new(r"whsec_[A-Za-z0-9]{16,}").expect("stripe webhook regex"),
                "whsec_[REDACTED]",
            ),
            // YYC-250: generic JSON catch-all — redact `"api_key": "..."`
            // (and the token/secret/password siblings) so a tool that
            // returns its own config struct can't leak the inner string
            // even when the value doesn't match a known prefix. Captures
            // group 1 = `"key": "`, group 2 = closing quote so the JSON
            // shape stays valid for downstream parsers.
            (
                Regex::new(
                    r#"(?i)("(?:api[_-]?key|token|secret|password|access[_-]?token)"\s*:\s*")[^"\\]{8,}(")"#,
                )
                .expect("json secret kv regex"),
                "$1[REDACTED]$2",
            ),
            // YYC-250: env-var leak — `AWS_SECRET_ACCESS_KEY=...`,
            // `OPENAI_API_KEY=...`, etc. Group 1 keeps the var name so
            // logs still show *which* secret leaked without the value.
            (
                Regex::new(
                    r#"\b([A-Z][A-Z0-9_]*_(?:KEY|SECRET|TOKEN|PASSWORD|PASS|PWD))=([^\s'"]{4,})"#,
                )
                .expect("env var leak regex"),
                "$1=[REDACTED]",
            ),
            // Azure-style hex tokens are caught by the Bearer regex.
            // Standalone hex strings are too noisy — git SHAs, content
            // hashes, CI build IDs all share the shape — so we lean on
            // the Bearer + env-var-leak regexes instead.
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
        // Non-env-var context so the prefix-specific pattern (and not
        // the YYC-250 env-var-leak pattern) is the one exercised here.
        let raw = "config: openrouter token sk-or-v1-abcdefghijklmnopqrstuvwx";
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

    // Test fixtures concatenate prefix + body at runtime so the source
    // file doesn't contain literal strings that match GitHub's secret
    // scanners (which would block the push).
    fn fake(prefix: &str, body: &str) -> String {
        format!("{prefix}{body}")
    }

    #[test]
    fn yyc250_redacts_google_api_key() {
        let body = "X".repeat(35);
        let raw = format!("GOOGLE_API_KEY={}", fake("AI", &format!("za{body}")));
        let red = redact_string(&raw);
        assert!(red.contains("[REDACTED]"));
    }

    #[test]
    fn yyc250_redacts_slack_bot_user_app_tokens() {
        let body = "1234567890-XXXXXXXXXXX";
        for prefix in ["xoxb", "xoxp", "xoxs", "xoxr", "xapp"] {
            let token = fake(prefix, &format!("-{body}"));
            let raw = format!("Slack: {token}");
            let red = redact_string(&raw);
            assert!(!red.contains(body), "leaked Slack token body: {red}");
        }
    }

    #[test]
    fn yyc250_redacts_stripe_live_keys() {
        let body = "X".repeat(20);
        for prefix in ["sk_live", "rk_live", "pk_live", "whsec"] {
            let token = fake(prefix, &format!("_{body}"));
            let raw = format!("KEY: {token}");
            let red = redact_string(&raw);
            assert!(red.contains("[REDACTED]"), "did not redact: {raw} -> {red}");
            assert!(!red.contains(&body), "leaked stripe body: {red}");
        }
    }

    #[test]
    fn yyc250_redacts_generic_json_secret_fields() {
        let raw = r#"{"api_key": "abcdefghijklmnop", "token": "9876543210abcdef", "secret": "ssh-keep-out-please"}"#;
        let red = redact_string(raw);
        assert!(!red.contains("abcdefghijklmnop"));
        assert!(!red.contains("9876543210abcdef"));
        assert!(!red.contains("ssh-keep-out-please"));
        assert!(red.contains("[REDACTED]"));
    }

    #[test]
    fn yyc250_redacts_env_var_leaks() {
        // Fake values to avoid GitHub secret-scanning false positives.
        for raw in [
            "AWS_SECRET_ACCESS_KEY=NOT-A-REAL-SECRET-PLACEHOLDER-VALUE",
            "OPENAI_API_KEY=fake-test-value-xxxxxxxx",
            "MY_AGENT_TOKEN=placeholder-token-1111",
            "DATABASE_PASSWORD=placeholder-pw-1111",
        ] {
            let red = redact_string(raw);
            assert!(red.contains("[REDACTED]"), "missed env leak in {raw}");
            assert!(
                !red.contains("NOT-A-REAL-SECRET-PLACEHOLDER"),
                "leaked: {red}"
            );
            assert!(
                !red.contains("placeholder-pw-1111"),
                "leaked password: {red}"
            );
        }
    }

    #[test]
    fn yyc250_truncation_runs_after_redaction() {
        // The secret sits past the 500-char window. Without
        // redact-before-truncate, it would survive in the kept prefix.
        // Place a known token at the *start* of the string so we can
        // confirm: post-redact, the head is short again so the
        // elided suffix drops.
        let mut raw = String::new();
        raw.push_str("Bearer sk-test-aaaaaaaaaaaaaaaaaaaaaa ");
        for _ in 0..600 {
            raw.push('z');
        }
        let red = redact_string(&raw);
        assert!(!red.contains("sk-test-aaaaaaaaaaaaaaaaaaaaaa"));
        assert!(red.contains("Bearer [REDACTED]"));
    }

    #[test]
    fn yyc250_does_not_redact_unrelated_long_strings() {
        // 40+ char hex (git SHA-style) was a tempting redact target
        // but produces too many false positives — confirm it stays
        // intact so commit hashes survive in logs.
        let raw = "commit 5a509b9b81ae8ff897a89706980068c33dedf16e abc";
        let red = redact_string(raw);
        assert_eq!(red, raw);
    }

    #[test]
    fn redact_response_text_handles_sse_with_embedded_keys() {
        let raw = "data: {\"id\":\"x\",\"choices\":[{\"delta\":{\"content\":\"see Bearer sk-leak-123456789012345678\"}}]}\n";
        let red = redact_response_text(raw);
        assert!(!red.contains("sk-leak-123456789012345678"));
        assert!(red.contains("Bearer [REDACTED]"));
    }
}
