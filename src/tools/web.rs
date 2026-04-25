use crate::tools::{Tool, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
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
        let query = params["query"].as_str().ok_or_else(|| anyhow::anyhow!("query required"))?;

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
            results.push(DdgResult { title, url, snippet });
            if results.len() >= 5 {
                break;
            }
        }
    }
    results
}

fn urlencoding(s: &str) -> String {
    let mut encoded = String::new();
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{:02X}", byte)),
        }
    }
    encoded
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
        let url = params["url"].as_str().ok_or_else(|| anyhow::anyhow!("url required"))?;

        let client = reqwest::Client::builder()
            .user_agent("vulcan/0.1 (AI agent; personal use)")
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let (status, body) = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(ToolResult::err("Cancelled")),
            res = async {
                let response = client.get(url).send().await?;
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
        if text.len() > 5000 {
            result.push_str(&text[..5000]);
            result.push_str("\n\n... (truncated at 5000 chars)");
        } else {
            result.push_str(&text);
        }

        Ok(ToolResult::ok(result))
    }
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
                if html[html.len().saturating_sub(8)..].to_lowercase().contains("/script") {
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
                if lower.contains("<script") { in_script = true; in_tag = false; continue; }
                if lower.contains("<style") { in_style = true; in_tag = false; continue; }
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
