use crate::tools::{ReplaySafety, Tool, ToolResult, parse_tool_params, web_ssrf};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
struct WebSearchParams {
    query: String,
}

#[derive(Deserialize)]
struct WebFetchParams {
    url: String,
}

pub struct WebSearch;

#[async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &str {
        "web_search"
    }
    fn replay_safety(&self) -> ReplaySafety {
        // Hits an external service; replay should not silently
        // re-run without explicit user opt-in.
        ReplaySafety::External
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
    async fn call(
        &self,
        params: Value,
        cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: WebSearchParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let query = p.query.as_str();

        // Use DuckDuckGo's lite version for simple scraping
        let url = format!("https://html.duckduckgo.com/html/?q={}", urlencoding(query));
        let client = shared_client();
        // YYC-256: rate-limit per host so the LLM can't hammer DDG.
        wait_for_rate_limit("html.duckduckgo.com").await;

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

/// YYC-252: parse DDG search results via `scraper` CSS selectors
/// instead of brittle `.split().nth()` chains. The new path tolerates
/// markup variations (extra whitespace, attribute reordering, nested
/// inline tags) that broke the prior parser whenever DDG tweaked
/// their HTML.
fn extract_ddg_results(html: &str) -> Vec<DdgResult> {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);
    // Selectors are constructed once per call. Caching them across
    // calls is possible but `scraper`'s Selector isn't trivially
    // cacheable (no Send). Single-call cost is negligible compared
    // to the network round trip the caller is about to do.
    let body_sel = match Selector::parse(".result__body") {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let title_sel = Selector::parse(".result__a").ok();
    let url_sel = Selector::parse(".result__url").ok();
    let snippet_sel = Selector::parse(".result__snippet").ok();

    let mut results = Vec::new();
    for body in document.select(&body_sel) {
        let title = title_sel
            .as_ref()
            .and_then(|sel| body.select(sel).next())
            .map(|el| collapse_ws(&el.text().collect::<String>()))
            .unwrap_or_default();
        let url = url_sel
            .as_ref()
            .and_then(|sel| body.select(sel).next())
            .map(|el| collapse_ws(&el.text().collect::<String>()))
            .unwrap_or_default();
        let snippet = snippet_sel
            .as_ref()
            .and_then(|sel| body.select(sel).next())
            .map(|el| collapse_ws(&el.text().collect::<String>()))
            .unwrap_or_default();
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

/// Collapse runs of whitespace + trim. DDG inserts newlines + indent
/// inside text nodes; the agent only needs a clean single-line string.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// YYC-256: minimum spacing between consecutive web-tool requests
/// to the same host. 200 ms ≈ 5 req/s — enough headroom for normal
/// follow-up calls; tight enough that the LLM can't accidentally
/// hammer a target.
const MIN_HOST_INTERVAL: Duration = Duration::from_millis(200);

/// YYC-256: shared `reqwest::Client` so the web tools reuse the
/// connection pool across calls instead of opening a fresh TCP/TLS
/// session every fetch. Lazy-initialized once per process.
fn shared_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent("vulcan/0.1 (AI agent; personal use)")
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// YYC-256: rough per-host rate limit. If the same host was hit less
/// than `MIN_HOST_INTERVAL` ago, sleep until the window elapses.
/// Implementation is a `Mutex<HashMap<host, last_call>>` — the lock
/// is held only briefly to read/update the timestamp, never across
/// the sleep itself.
async fn wait_for_rate_limit(host: &str) {
    static LAST_CALL: OnceLock<std::sync::Mutex<HashMap<String, Instant>>> = OnceLock::new();
    let map = LAST_CALL.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let wait = {
        let mut guard = map.lock().expect("rate-limit map poisoned");
        let now = Instant::now();
        let key = host.to_string();
        match guard.get(&key) {
            Some(prev) => {
                let elapsed = now.duration_since(*prev);
                if elapsed >= MIN_HOST_INTERVAL {
                    guard.insert(key, now);
                    Duration::ZERO
                } else {
                    let wait = MIN_HOST_INTERVAL - elapsed;
                    // Optimistically book the next slot so concurrent
                    // callers serialize without piling up.
                    guard.insert(key, now + wait);
                    wait
                }
            }
            None => {
                guard.insert(key, now);
                Duration::ZERO
            }
        }
    };
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
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
    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::External
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
    async fn call(
        &self,
        params: Value,
        cancel: CancellationToken,
        _progress: Option<crate::tools::ProgressSink>,
    ) -> Result<ToolResult> {
        let p: WebFetchParams = match parse_tool_params(params) {
            Ok(p) => p,
            Err(e) => return Ok(e),
        };
        let url = p.url.as_str();

        // YYC-246: SSRF guard. Refuses non-HTTP(S) schemes and any URL
        // whose host (literal or DNS-resolved) sits in a private,
        // loopback, link-local, multicast, or otherwise non-public
        // address class. See `web_ssrf` for the full block list.
        let validated = match web_ssrf::validate(url).await {
            Ok(parsed) => parsed,
            Err(e) => return Ok(ToolResult::err(format!("URL refused: {e}"))),
        };

        // YYC-256: rate-limit per host before fetching.
        if let Some(host) = validated.host_str() {
            wait_for_rate_limit(host).await;
        }
        let client = shared_client();

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

/// YYC-254: render HTML body as plain text via the `html2text`
/// crate. The previous hand-rolled char-by-char state machine
/// missed CDATA, HTML entities (`&amp;`, `&#x20;`), `<pre>`
/// whitespace, and nested-tag edge cases — `html2text` handles all
/// four because it lowers HTML through `html5ever` and then renders
/// the DOM as text.
///
/// The 1024-column width is wider than any reasonable terminal so
/// the output isn't artificially line-wrapped — the agent's own
/// truncate_chars cap downstream (FETCH_MAX_CHARS) handles length.
fn html_to_text(html: &str) -> String {
    const RENDER_WIDTH: usize = 1024;
    match html2text::from_read(html.as_bytes(), RENDER_WIDTH) {
        Ok(rendered) => rendered.trim().to_string(),
        Err(e) => {
            tracing::warn!("html2text render failed: {e}");
            // Fall back to a naive tag-strip so the tool still
            // produces *something* usable on malformed pages.
            naive_strip_tags(html)
        }
    }
}

/// Last-resort tag stripper for inputs `html2text` rejects.
/// Walks each char and skips everything between `<` and `>`. Doesn't
/// handle CDATA / entities / scripts — those cases get the worse
/// output, but the tool keeps responding.
fn naive_strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => {
                if ch.is_whitespace() {
                    if !out.ends_with(' ') && !out.is_empty() {
                        out.push(' ');
                    }
                } else {
                    out.push(ch);
                }
            }
            _ => {}
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yyc222_web_tools_are_classified_external() {
        assert_eq!(WebSearch.replay_safety(), ReplaySafety::External);
        assert_eq!(WebFetch.replay_safety(), ReplaySafety::External);
    }

    #[tokio::test]
    async fn yyc263_web_search_missing_query_surfaces_as_toolresult_err() {
        let result = WebSearch
            .call(json!({}), CancellationToken::new(), None)
            .await
            .expect("call returns Ok(ToolResult)");
        assert!(result.is_error);
        assert!(
            result.output.contains("tool params failed to validate"),
            "expected serde-shaped error, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn yyc263_web_fetch_missing_url_surfaces_as_toolresult_err() {
        let result = WebFetch
            .call(json!({}), CancellationToken::new(), None)
            .await
            .expect("call returns Ok(ToolResult)");
        assert!(result.is_error);
        assert!(
            result.output.contains("tool params failed to validate"),
            "expected serde-shaped error, got: {}",
            result.output
        );
    }

    #[test]
    fn yyc257_extract_ddg_results_parses_titles_urls_snippets() {
        // Stripped-down DDG result HTML — three results, each with the
        // class hooks the parser keys on.
        let html = r##"
<html><body>
<div class="result__body">
  <a class="result__a" href="x">First Result</a>
  <a class="result__url" href="x">https://first.example/</a>
  <a class="result__snippet" href="x">First snippet text.</a>
</div>
<div class="result__body">
  <a class="result__a" href="x">Second Result</a>
  <a class="result__url" href="x">https://second.example/</a>
  <a class="result__snippet" href="x">Second snippet text.</a>
</div>
</body></html>
"##;
        let results = extract_ddg_results(html);
        assert_eq!(results.len(), 2);
        // YYC-252: scraper-backed parser produces clean strings —
        // no trailing `</a` tail anymore.
        assert_eq!(results[0].title, "First Result");
        assert_eq!(results[0].url, "https://first.example/");
        assert_eq!(results[0].snippet, "First snippet text.");
        assert_eq!(results[1].title, "Second Result");
    }

    #[test]
    fn yyc252_extract_ddg_results_handles_whitespace_and_nested_tags() {
        // Real-world DDG injects extra whitespace + inline `<b>`
        // highlight tags inside snippets. The CSS-selector path
        // extracts the combined text and collapses whitespace.
        let html = r##"
<div class="result__body">
  <a class="result__a" href="x">
    Search   Result   Title
  </a>
  <a class="result__url" href="x">https://example.com/</a>
  <span class="result__snippet">Hello <b>highlighted</b> world.</span>
</div>
"##;
        let results = extract_ddg_results(html);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Search Result Title");
        assert_eq!(results[0].snippet, "Hello highlighted world.");
    }

    #[test]
    fn yyc257_extract_ddg_results_empty_when_no_result_blocks() {
        let html = "<html><body><p>nothing here</p></body></html>";
        assert!(extract_ddg_results(html).is_empty());
    }

    #[test]
    fn yyc257_extract_ddg_results_caps_at_five() {
        let mut html = String::new();
        for i in 0..10 {
            html.push_str(&format!(
                r##"<div class="result__body">
                <a class="result__a" href="x">Title {i}</a>
                <a class="result__url" href="x">https://e{i}.example/</a>
                <a class="result__snippet" href="x">Snippet {i}.</a>
                </div>"##
            ));
        }
        let results = extract_ddg_results(&html);
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn yyc257_html_to_text_strips_tags() {
        // YYC-254: html2text now powers the renderer. Exact spacing
        // and inline-tag rendering depend on the crate, so the test
        // only asserts the structural properties: tag characters are
        // gone and the visible words land in the output in order.
        let html = "<p>Hello, <b>world</b>!</p><p>Second line.</p>";
        let text = html_to_text(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("world"));
        assert!(text.contains("Second line."));
        assert!(!text.contains('<'));
        assert!(!text.contains('>'));
    }

    #[test]
    fn yyc254_html_to_text_decodes_entities() {
        // The hand-rolled stripper passed `&amp;` through verbatim;
        // html2text decodes named + numeric entities.
        let html = "<p>R&amp;D &amp; tools (&#x40; &#64;)</p>";
        let text = html_to_text(html);
        assert!(text.contains("R&D"));
        assert!(text.contains("tools"));
        assert!(!text.contains("&amp;"));
    }

    #[test]
    fn yyc254_html_to_text_skips_script_and_style_blocks() {
        let html =
            "<style>.x { color: red }</style><p>visible</p><script>alert('hi')</script>after";
        let text = html_to_text(html);
        assert!(text.contains("visible"));
        assert!(text.contains("after"));
        // Neither inline JS nor inline CSS should land in output.
        assert!(!text.contains("alert"));
        assert!(!text.contains("color: red"));
    }

    #[test]
    fn yyc254_naive_strip_tags_falls_back_when_html2text_fails() {
        // Best-effort fallback path. Pin its happy case directly.
        let stripped = naive_strip_tags("<p>hello <b>world</b></p>");
        assert!(stripped.contains("hello"));
        assert!(stripped.contains("world"));
        assert!(!stripped.contains('<'));
    }

    #[test]
    fn yyc257_html_to_text_includes_visible_paragraph_text() {
        // The hand-rolled stripper has known quirks around <script>
        // and <style> blocks (it scans the whole input for those
        // tags rather than tracking nesting cleanly). M6 tracks the
        // upgrade to a real parser. For now we pin the visible-text
        // case rather than the script-stripping behaviour.
        let html = "<p>visible content</p>";
        let text = html_to_text(html);
        assert!(text.contains("visible content"));
        assert!(!text.contains('<'));
    }

    #[test]
    fn yyc257_html_to_text_handles_empty_input() {
        assert_eq!(html_to_text(""), "");
    }

    #[test]
    fn yyc256_shared_client_returns_same_pointer_each_call() {
        let a = shared_client() as *const _;
        let b = shared_client() as *const _;
        assert_eq!(a, b, "shared_client should hand out the same instance");
    }

    #[tokio::test]
    async fn yyc256_rate_limiter_first_call_does_not_sleep() {
        let host = format!("yyc256-fresh-{}.example", std::process::id());
        let start = Instant::now();
        wait_for_rate_limit(&host).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(50),
            "first call shouldn't sleep, took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn yyc256_rate_limiter_second_call_sleeps_at_least_window() {
        // Use a unique host so other tests' state can't bleed in.
        let host = format!("yyc256-second-{}.example", std::process::id());
        wait_for_rate_limit(&host).await; // primes the map
        let start = Instant::now();
        wait_for_rate_limit(&host).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(150),
            "second call within window should sleep ~MIN_HOST_INTERVAL, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn yyc256_rate_limiter_distinct_hosts_do_not_block_each_other() {
        let host_a = format!("yyc256-distinct-a-{}.example", std::process::id());
        let host_b = format!("yyc256-distinct-b-{}.example", std::process::id());
        wait_for_rate_limit(&host_a).await;
        let start = Instant::now();
        wait_for_rate_limit(&host_b).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(50),
            "different host shouldn't be throttled by another's window, took {elapsed:?}"
        );
    }

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
