//! Web search tool for ZeroZero .
//!
//! Performs web searches via an HTTP API and returns results as text.
//! Uses a configurable search endpoint (default: DuckDuckGo HTML endpoint
//! which requires no API key). For production use, set ZZ_SEARCH_API_URL
//! and ZZ_SEARCH_API_KEY for a JSON-based search API.

use crate::{Tool, ToolCategory};

pub struct WebSearchTool {
    client: reqwest::Client,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for WebSearchTool {
    fn is_read_only(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web for information. Returns a list of results with \
         titles, URLs, and snippets. Useful for finding documentation, \
         error solutions, or current information. \
         Required: `query` (search terms). \
         Optional: `num_results` (default 5, max 10)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query string"
                },
                "num_results": {
                    "type": "integer",
                    "description": "Number of results to return (default 5, max 10)",
                    "default": 5
                }
            },
            "required": ["query"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Web
    }

    fn when_to_use(&self) -> Option<&str> {
        Some(
            "you need current information, documentation, or solutions that may not be in the local codebase",
        )
    }

    fn when_not_to_use(&self) -> Option<&str> {
        Some(
            "the answer is likely in the local codebase — use `grep`/`read_file` first (faster, no network)",
        )
    }

    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required field 'query'"))?;
        let num_results = args
            .get("num_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .min(10) as usize;

        // Check for custom search API via env vars.
        let api_url = std::env::var("ZZ_SEARCH_API_URL").ok();
        let api_key = std::env::var("ZZ_SEARCH_API_KEY").ok();

        if let (Some(url), Some(key)) = (api_url, api_key) {
            // Use custom JSON search API.
            return self.search_json_api(&url, &key, query, num_results).await;
        }

        // Fallback: DuckDuckGo HTML endpoint (no API key needed).
        self.search_duckduckgo(query, num_results).await
    }
}

impl WebSearchTool {
    /// Search using a JSON API (e.g., SerpAPI, Brave Search API).
    async fn search_json_api(
        &self,
        url: &str,
        key: &str,
        query: &str,
        num_results: usize,
    ) -> anyhow::Result<String> {
        let full_url = format!("{url}?q={}&num={num_results}", urlencode(query),);
        let response = self
            .client
            .get(&full_url)
            .header("Authorization", format!("Bearer {key}"))
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("search API error {status}: {text}");
        }
        let json: serde_json::Value = response.json().await?;
        Ok(format_json_results(&json, num_results))
    }

    /// Search using DuckDuckGo HTML endpoint (no API key required).
    async fn search_duckduckgo(&self, query: &str, num_results: usize) -> anyhow::Result<String> {
        let url = format!("https://html.duckduckgo.com/html/?q={}", urlencode(query),);
        let response = self
            .client
            .get(&url)
            .header("User-Agent", "Mozilla/5.0 (compatible; ZeroZero/0.1)")
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("DuckDuckGo error {status}: {text}");
        }
        let html = response.text().await?;
        Ok(parse_duckduckgo_html(&html, num_results))
    }
}

/// Format JSON API search results into readable text.
/// Expects a JSON object with a `results` array containing objects with
/// `title`, `url`, and `snippet` fields.
fn format_json_results(json: &serde_json::Value, max: usize) -> String {
    let mut output = String::new();
    if let Some(results) = json.get("results").and_then(|r| r.as_array()) {
        for (i, result) in results.iter().take(max).enumerate() {
            let title = result
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("(no title)");
            let url = result
                .get("url")
                .and_then(|u| u.as_str())
                .unwrap_or("(no url)");
            let snippet = result
                .get("snippet")
                .and_then(|s| s.as_str())
                .unwrap_or("(no snippet)");
            output.push_str(&format!(
                "{}. {}\n   URL: {}\n   {}\n\n",
                i + 1,
                title,
                url,
                snippet
            ));
        }
    }
    if output.is_empty() {
        output.push_str("No results found.\n");
    }
    output
}

/// Parse DuckDuckGo HTML results page into readable text.
/// Extracts result titles, URLs, and snippets from the HTML.
fn parse_duckduckgo_html(html: &str, max: usize) -> String {
    let mut results = Vec::new();
    let mut count = 0;

    // DuckDuckGo HTML results are in <a class="result__a" href="...">title</a>
    // and snippets in <a class="result__snippet">...</a>
    // We use simple string searching (not a full HTML parser) to avoid
    // adding an HTML parsing dependency.
    for segment in html.split("result__body") {
        if count >= max {
            break;
        }
        // Extract URL from href="..."
        let url = extract_between(segment, "href=\"", "\"")
            .or_else(|| extract_between(segment, "uddg=", "&"))
            .unwrap_or_default();
        // Extract title from result__a
        let title = extract_between(segment, "result__a", "</a>")
            .map(|s| strip_html_tags(&s))
            .unwrap_or_default();
        // Extract snippet
        let snippet = extract_between(segment, "result__snippet", "</a>")
            .or_else(|| extract_between(segment, "result__snippet\">", "</td>"))
            .map(|s| strip_html_tags(&s))
            .unwrap_or_default();

        if !title.is_empty() {
            results.push(format!(
                "{}. {}\n   URL: {}\n   {}\n\n",
                count + 1,
                title.trim(),
                url.trim(),
                snippet.trim()
            ));
            count += 1;
        }
    }

    if results.is_empty() {
        return "No results found.\n".to_string();
    }
    results.join("")
}

/// Extract the substring between `start_marker` and `end_marker` after the
/// first occurrence of `start_marker`. Returns None if not found.
fn extract_between(text: &str, start_marker: &str, end_marker: &str) -> Option<String> {
    let start_idx = text.find(start_marker)?;
    let after_start = &text[start_idx + start_marker.len()..];
    // Skip to next '>' if the marker is a tag name (not a full tag)
    let content_start = if start_marker.ends_with('>') || start_marker.contains('"') {
        0
    } else {
        after_start.find('>')? + 1
    };
    let content = &after_start[content_start..];
    let end_idx = content.find(end_marker)?;
    Some(content[..end_idx].to_string())
}

/// Strip HTML tags from a string.
fn strip_html_tags(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            result.push(ch);
        }
    }
    // Decode common HTML entities
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

/// Percent-encode a string for use in a URL query parameter.
fn urlencode(s: &str) -> String {
    let mut result = String::with_capacity(s.len() * 3);
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            b' ' => result.push('+'),
            _ => result.push_str(&format!("%{byte:02X}")),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_html_tags() {
        assert_eq!(strip_html_tags("<b>hello</b>"), "hello");
        assert_eq!(strip_html_tags("no tags"), "no tags");
        assert_eq!(strip_html_tags("<a href=\"x\">link</a>"), "link");
        assert_eq!(strip_html_tags("a &amp; b"), "a & b");
    }

    #[test]
    fn test_extract_between() {
        let text = "prefix href=\"http://example.com\" suffix";
        let result = extract_between(text, "href=\"", "\"");
        assert_eq!(result, Some("http://example.com".to_string()));
    }

    #[test]
    fn test_extract_between_not_found() {
        let text = "no markers here";
        assert_eq!(extract_between(text, "href=\"", "\""), None);
    }

    #[test]
    fn test_format_json_results() {
        let json = serde_json::json!({
            "results": [
                {"title": "Rust docs", "url": "https://doc.rust-lang.org", "snippet": "The Rust programming language"},
                {"title": "Crates.io", "url": "https://crates.io", "snippet": "Rust package registry"}
            ]
        });
        let output = format_json_results(&json, 10);
        assert!(output.contains("Rust docs"));
        assert!(output.contains("https://doc.rust-lang.org"));
        assert!(output.contains("Crates.io"));
    }

    #[test]
    fn test_format_json_results_empty() {
        let json = serde_json::json!({"results": []});
        let output = format_json_results(&json, 10);
        assert_eq!(output, "No results found.\n");
    }

    #[test]
    fn test_format_json_results_max_limit() {
        let json = serde_json::json!({
            "results": [
                {"title": "1", "url": "u1", "snippet": "s1"},
                {"title": "2", "url": "u2", "snippet": "s2"},
                {"title": "3", "url": "u3", "snippet": "s3"}
            ]
        });
        let output = format_json_results(&json, 2);
        assert!(output.contains("1. "));
        assert!(output.contains("2. "));
        assert!(!output.contains("3. "));
    }

    #[test]
    fn test_parse_duckduckgo_html_empty() {
        let output = parse_duckduckgo_html("<html><body>no results</body></html>", 5);
        assert_eq!(output, "No results found.\n");
    }

    #[test]
    fn test_parse_duckduckgo_html_with_results() {
        let html = r#"
        <div class="result__body">
          <a class="result__a" href="http://example.com">Example Site</a>
          <a class="result__snippet">This is a snippet</a>
        </div>
        <div class="result__body">
          <a class="result__a" href="http://rust-lang.org">Rust Language</a>
          <a class="result__snippet">Systems programming language</a>
        </div>
        "#;
        let output = parse_duckduckgo_html(html, 5);
        assert!(
            output.contains("Example Site") || output.contains("Rust Language"),
            "Should extract at least one result. Got: {output}"
        );
    }

    #[tokio::test]
    async fn test_web_search_missing_query() {
        let tool = WebSearchTool::new();
        let result = tool.execute(&serde_json::json!({})).await;
        assert!(result.is_err(), "Should error on missing query");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("query"),
            "Error should mention query. Got: {err}"
        );
    }

    #[test]
    fn test_tool_name_and_description() {
        let tool = WebSearchTool::new();
        assert_eq!(tool.name(), "web_search");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["query"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("query"))
        );
    }

    #[test]
    fn test_urlencode() {
        assert_eq!(urlencode("hello"), "hello");
        assert_eq!(urlencode("hello world"), "hello+world");
        assert_eq!(urlencode("a&b"), "a%26b");
        assert_eq!(urlencode("rust lang"), "rust+lang");
    }
}
