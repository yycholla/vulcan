use crate::tools::{Tool, ToolResult, web_ssrf};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

pub struct WebSearch;

#[async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &str {
        "web_search"
    }
    fn description(&self) -> &str {
        "Search the web for information. Returns up to 5 results with titles, URLs, and descriptions."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "The search query" }
            },
            "required": ["query"]
        })
    }
    async fn call(&self, params: Value, cancel: CancellationToken) -> Result<ToolResult> {
        let query = params["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("query required"))?;

        // Use DuckDuckGo's lite version for simple scraping
        let url = format!("https://html.duckduckgo.com/html/?q={}", urlencoding(query));
        let client = reqwest::Client::builder()
            .user_agent("vulcan/0.1 (AI agent; personal use)")
            .build()?;

        let html = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(ToolResult::err("Cancelled")),
            res = async {
                let response = client.get(&url).send().await?;
                response.text().await
            } => res?,
        };

        // Simple extraction of result links from DuckDuckGo HTML
        let results = extract_ddg_results(&html);

        let output = if results.is_empty() {
            "No results found.".to_string()
        } else {
            results
                .iter()
                .enumerate()
                .map(|(i, r)| format!("{}. [{}]({})\n   {}", i + 1, r.title, r.url, r.snippet))
                .collect::<Vec<_>>()
                .join("\n\n")
        };
        Ok(ToolResult::ok(output))
    }
}

struct DdgResult {
    title: String,
    url: String,
    snippet: String,
}

fn extract_ddg_results(html: &str) -> Vec<DdgResult> {
    let mut results = Vec::new();
    // Simple parser — looks for result_<id> divs in DuckDuckGo HTML
    for chunk in html.split(r##"<div class="result__body">"##).skip(1) {
        // Extract title
        let title = chunk
            .split(r##"class="result__a"##)
            .nth(1)
            .and_then(|s| s.split('>').nth(1))
            .and_then(|s| s.split("</a>").next())
            .unwrap_or("")
            .trim()
            .to_string();

        // Extract URL
        let url = chunk
            .split(r##"class="result__url"##)
            .nth(1)
            .and_then(|s| s.split('>').nth(1))
            .and_then(|s| s.split("</a>").next())
            .unwrap_or("")
            .trim()
            .to_string();

        // Extract snippet
        let snippet = chunk
            .split(r##"class="result__snippet"##)
            .nth(1)
            .and_then(|s| s.split('>').nth(1))
            .and_then(|s| s.split("</a>").next())
            .unwrap_or("")
            .trim()
            .to_string();

        if !title.is_empty() {
            results.push(DdgResult {
                title,
                url,
                snippet,
            });
            if results.len() >= 5 {
                break;
            }
        }
    }
    results
}

/// YYC-253: percent-encode a query-string value using the vetted
/// `percent-encoding` crate. Matches the prior hand-rolled function's
/// shape (`+` for space, percent-encode everything else outside the
/// RFC 3986 unreserved set) so existing search backends parse the
/// query correctly.
fn urlencoding(s: &str) -> String {
    use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
    // RFC 3986 reserved + non-unreserved chars. The unreserved set is
    // `A-Z` / `a-z` / `0-9` / `-` / `_` / `.` / `~`.
    const QUERY_SET: &AsciiSet = &CONTROLS
        .add(b' ') // ' ' becomes %20 here, then we swap to '+' below.
        .add(b'"')
        .add(b'#')
        .add(b'$')
        .add(b'%')
        .add(b'&')
        .add(b'\'')
        .add(b'(')
        .add(b')')
        .add(b'*')
        .add(b'+')
        .add(b',')
        .add(b'/')
        .add(b':')
        .add(b';')
        .add(b'<')
        .add(b'=')
        .add(b'>')
        .add(b'?')
        .add(b'@')
        .add(b'[')
        .add(b'\\')
        .add(b']')
        .add(b'^')
        .add(b'`')
        .add(b'{')
        .add(b'|')
        .add(b'}');
    // Swap %20 → '+' for query-string convention.
    utf8_percent_encode(s, QUERY_SET)
        .to_string()
        .replace("%20", "+")
}

pub struct WebFetch;

#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &str {
        "web_fetch"
    }
    fn description(&self) -> &str {
        "Fetch the content of a URL and extract it as markdown text."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "URL to fetch" }
            },
            "required": ["url"]
        })
    }
    async fn call(&self, params: Value, cancel: CancellationToken) -> Result<ToolResult> {
        let url = params["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("url required"))?;

        // YYC-246: SSRF guard. Refuses non-HTTP(S) schemes and any URL
        // whose host (literal or DNS-resolved) sits in a private,
        // loopback, link-local, multicast, or otherwise non-public
        // address class. See `web_ssrf` for the full block list.
        let validated = match web_ssrf::validate(url).await {
            Ok(parsed) => parsed,
            Err(e) => return Ok(ToolResult::err(format!("URL refused: {e}"))),
        };

        let client = reqwest::Client::builder()
            .user_agent("vulcan/0.1 (AI agent; personal use)")
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let (status, body) = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(ToolResult::err("Cancelled")),
            res = async {
                let response = client.get(validated.as_str()).send().await?;
                let status = response.status();
                if !status.is_success() {
                    return Err(anyhow::anyhow!("HTTP {status} fetching {url}"));
                }
                let body = response.text().await?;
                Ok::<_, anyhow::Error>((status, body))
            } => res?,
        };

        // Trim page content
        let text = html_to_text(&body);

        let mut result = format!("URL: {url}\nStatus: {status}\n\n");
        let (body_text, truncated) = truncate_chars(&text, FETCH_MAX_CHARS);
        result.push_str(&body_text);
        if truncated {
            result.push_str("\n\n... (truncated at 5000 chars)");
        }

        Ok(ToolResult::ok(result))
    }
}

/// YYC-255: cap on `web_fetch` output text length, expressed in
/// Unicode scalar values. Slicing by byte offset would panic on
/// non-ASCII content; iterating over `chars()` walks scalar values.
const FETCH_MAX_CHARS: usize = 5000;

/// YYC-255: truncate `text` to at most `max` chars (Unicode scalar
/// values). Returns `(truncated_text, was_truncated)` so the caller
/// only renders the elision marker when it actually elided.
fn truncate_chars(text: &str, max: usize) -> (String, bool) {
    let count = text.chars().count();
    if count <= max {
        return (text.to_string(), false);
    }
    (text.chars().take(max).collect(), true)
}

fn html_to_text(html: &str) -> String {
    // Simple HTML-to-text extraction — remove tags, normalize whitespace
    let mut text = String::new();
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;

    for ch in html.chars() {
        if in_script {
            if ch == '<' {
                // Check for closing script tag
                if html[html.len().saturating_sub(8)..]
                    .to_lowercase()
                    .contains("/script")
                {
                    in_script = false;
                }
            }
            continue;
        }
        if in_style {
            if ch == '<' {
                in_style = false;
            }
            continue;
        }
        match ch {
            '<' => {
                in_tag = true;
                // Detect script/style blocks to skip
                let lower = html[..].to_lowercase();
                if lower.contains("<script") {
                    in_script = true;
                    in_tag = false;
                    continue;
                }
                if lower.contains("<style") {
                    in_style = true;
                    in_tag = false;
                    continue;
                }
            }
            '>' if in_tag => {
                in_tag = false;
            }
            _ if !in_tag => {
                // Normalize whitespace
                if ch.is_whitespace() {
                    if !text.ends_with(' ') && !text.is_empty() {
                        text.push(' ');
                    }
                } else {
                    text.push(ch);
                }
            }
            _ => {}
        }
    }

    text.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yyc253_urlencoding_passes_unreserved_through() {
        // RFC 3986 unreserved set survives untouched.
        assert_eq!(urlencoding("AaZz09-_.~"), "AaZz09-_.~".to_string());
    }

    #[test]
    fn yyc253_urlencoding_swaps_space_to_plus() {
        assert_eq!(urlencoding("hello world"), "hello+world");
        assert_eq!(urlencoding("a b c"), "a+b+c");
    }

    #[test]
    fn yyc253_urlencoding_percent_encodes_reserved() {
        assert_eq!(urlencoding("a&b"), "a%26b");
        assert_eq!(urlencoding("?q=x"), "%3Fq%3Dx");
        assert_eq!(urlencoding("/path"), "%2Fpath");
    }

    #[test]
    fn yyc253_urlencoding_handles_unicode() {
        // Multi-byte UTF-8 must each percent-encode as separate bytes.
        let out = urlencoding("héllo");
        // 'é' = 0xC3 0xA9 in UTF-8.
        assert_eq!(out, "h%C3%A9llo");
    }

    #[test]
    fn yyc253_urlencoding_empty_string_passthrough() {
        assert_eq!(urlencoding(""), "");
    }

    #[test]
    fn truncate_chars_passes_through_when_under_cap() {
        let (out, truncated) = truncate_chars("hello", 100);
        assert_eq!(out, "hello");
        assert!(!truncated);
    }

    #[test]
    fn truncate_chars_caps_at_max() {
        let raw = "x".repeat(200);
        let (out, truncated) = truncate_chars(&raw, 50);
        assert_eq!(out.chars().count(), 50);
        assert!(truncated);
    }

    #[test]
    fn truncate_chars_does_not_panic_on_emoji_at_byte_boundary() {
        // Construct a string where the byte-offset cap (4998) lands
        // inside the multibyte sequence of an emoji at position
        // 4999/5000. Slicing by `&s[..5000]` would panic; the char-aware
        // helper must succeed and return exactly 5000 chars.
        let mut raw = String::new();
        for _ in 0..4999 {
            raw.push('a');
        }
        raw.push('🦀'); // 4 bytes; the only non-ASCII char.
        let (out, truncated) = truncate_chars(&raw, 5000);
        assert_eq!(out.chars().count(), 5000);
        assert!(!truncated, "exactly-cap input should not flag truncation");
    }

    #[test]
    fn truncate_chars_handles_cjk_past_cap() {
        let raw: String = "你".repeat(6000);
        let (out, truncated) = truncate_chars(&raw, FETCH_MAX_CHARS);
        assert_eq!(out.chars().count(), FETCH_MAX_CHARS);
        assert!(truncated);
        // Output must remain valid UTF-8 — implicit, but assert it
        // doesn't panic when round-tripped.
        let _ = out.as_bytes();
    }

    #[test]
    fn truncate_chars_zero_max_returns_empty_when_input_nonempty() {
        let (out, truncated) = truncate_chars("hi", 0);
        assert!(out.is_empty());
        assert!(truncated);
    }
}
