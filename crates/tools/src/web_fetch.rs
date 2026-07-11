//! Web fetch tool for ZeroZero parity with Codex `web_fetch`).
//!
//! Fetches the content of a URL over HTTP and returns it as text. By default
//! the HTML is stripped down to plain text (a lightweight, dependency-free
//! transform); pass `markdown: false` to receive the raw response body.
//!
//! NOTE: like `web_search`, this tool runs inside the agent's own process and
//! is therefore NOT subject to the sandbox network-namespace isolation (which
//! only wraps spawned shell commands via bubblewrap). Network egress for the
//! agent process itself is governed by the host environment.

use crate::{Tool, ToolCategory};

pub struct WebFetchTool {
    client: reqwest::Client,
}

impl WebFetchTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("zerozero/1.0")
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for WebFetchTool {
    fn is_read_only(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch the content of a URL and return it as text. By default the page \
         HTML is stripped to readable plain text; set `markdown: false` to get \
         the raw response body. Useful for reading documentation, RFCs, source \
         files, or any web page the agent needs to read. \
         Required: `url` (the page to fetch). \
         Optional: `markdown` (bool, default true — strip HTML to plain text), \
         `max_chars` (int, default 8000, max 64000 — truncate long pages)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "URL to fetch (http or https)"
                },
                "markdown": {
                    "type": "boolean",
                    "description": "Strip HTML to plain text (default true)",
                    "default": true
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Max characters to return (default 8000, max 64000)",
                    "default": 8000
                }
            },
            "required": ["url"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Web
    }

    fn when_to_use(&self) -> Option<&str> {
        Some(
            "you have a specific URL and need to read its content (docs, RFCs, source files, error pages)",
        )
    }

    fn when_not_to_use(&self) -> Option<&str> {
        Some("you don't have a URL yet — use `web_search` to find one first")
    }

    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing required field 'url'"))?;

        let as_markdown = args
            .get("markdown")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let max_chars = args
            .get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(8000)
            .min(64000) as usize;

        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("request to {url} failed: {e}"))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow::anyhow!(
                "HTTP {} while fetching {url}",
                status.as_u16()
            ));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("reading body of {url} failed: {e}"))?;

        let mut content = if as_markdown { strip_html(&body) } else { body };

        if content.chars().count() > max_chars {
            let truncated: String = content.chars().take(max_chars).collect();
            content = format!("{truncated}\n...(truncated at {max_chars} chars)");
        }

        Ok(content)
    }
}

/// Lightweight HTML→plain-text transform (no external dependencies).
///
/// Removes `<script>`/`<style>` blocks, strips remaining tags, decodes a
/// handful of common entities, and collapses whitespace. This is intentionally
/// simple — good enough for an agent to read page text, not a full renderer.
fn strip_html(html: &str) -> String {
    // Drop script/style contents entirely.
    let mut s = String::with_capacity(html.len());
    let mut in_skip = false;
    let mut tag_depth = 0i32;
    let bytes = html.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'<' {
            // Peek at the tag name to detect script/style.
            let rest = &html[i..];
            let end = rest.find('>').unwrap_or(rest.len() - 1);
            let tag = &rest[..=end];
            let lower = tag.to_ascii_lowercase();
            if lower.starts_with("<script") || lower.starts_with("<style") {
                in_skip = true;
            }
            if in_skip {
                if lower.starts_with("</script") || lower.starts_with("</style") {
                    in_skip = false;
                }
                i += 1;
                continue;
            }
            // Normal tag: replace with a space so words don't merge.
            s.push(' ');
            tag_depth += 1;
            i += 1;
            continue;
        }
        if in_skip {
            i += 1;
            continue;
        }
        if bytes[i] == b'>' {
            tag_depth -= 1;
            i += 1;
            continue;
        }
        s.push(bytes[i] as char);
        i += 1;
    }
    let _ = tag_depth;

    // Decode a few common entities.
    let s = s
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ");

    // Collapse runs of whitespace into single spaces, trim.
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(ch);
            prev_ws = false;
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_web_fetch_missing_url() {
        let tool = WebFetchTool::new();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(tool.execute(&serde_json::json!({})));
        assert!(result.is_err(), "should error on missing url");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("url"), "error should mention url. Got: {err}");
    }

    #[test]
    fn test_tool_name_and_schema() {
        let tool = WebFetchTool::new();
        assert_eq!(tool.name(), "web_fetch");
        assert!(!tool.description().is_empty());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["url"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("url"))
        );
        // markdown defaults to true (boolean property present).
        assert!(schema["properties"]["markdown"].is_object());
    }

    #[test]
    fn test_strip_html_basic() {
        let html = "<html><head><style>.x{color:red}</style></head>\
            <body><p>Hello <b>world</b></p><script>alert(1)</script>\
            <div>Line&nbsp;two &amp; end</div></body></html>";
        let text = strip_html(html);
        assert!(text.contains("Hello"), "got: {text}");
        assert!(text.contains("world"), "got: {text}");
        assert!(text.contains("Line two & end"), "got: {text}");
        // script content must be gone
        assert!(!text.contains("alert"), "script leaked: {text}");
        // tags must be gone
        assert!(!text.contains('<'), "tag leaked: {text}");
    }

    #[test]
    fn test_strip_html_entities() {
        let text = strip_html("a &lt;b&gt; c &quot;d&quot; &#39;e&#39;");
        assert_eq!(text, "a <b> c \"d\" 'e'");
    }
}
