//! Hand-rolled SSE parser for OpenAI Chat Completions streaming responses.
//!
//! OpenAI SSE format: lines separated by `\n`, data lines start with
//! `data: `, end marker is `data: [DONE]`. Each data payload is a JSON
//! object with `choices[0].delta.content` and/or
//! `choices[0].delta.tool_calls`.

/// Parsed SSE event from a streaming response line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEvent {
    /// Content delta (token-by-token text).
    Content(String),
    /// Reasoning / extended-thinking delta (display separately in TUI).
    Reasoning(String),
    /// Complete thinking block with cryptographic signature (Anthropic
    /// extended thinking). Emitted at `content_block_stop` for thinking
    /// blocks. The `thinking` text and `signature` must be sent back
    /// verbatim in multi-turn conversations.
    ThinkingBlock { thinking: String, signature: String },
    /// Redacted thinking block data (Anthropic safety-flagged, encrypted).
    /// Must be sent back verbatim in multi-turn conversations.
    RedactedThinking(String),
    /// Complete tool call — id, function name, and accumulated arguments.
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    /// `[DONE]` marker — stream finished.
    Done,
}

/// Parse one SSE line and extract the event if present.
///
/// Returns `Some(SseEvent::Content(text))` for content deltas.
/// Returns `Some(SseEvent::Done)` for the `[DONE]` marker.
/// Returns `None` for:
/// - Empty lines (event separators)
/// - Lines without `data: ` prefix
/// - Data events with only tool_calls (partial — accumulation handled by caller)
/// - Data events without content or tool_calls (e.g. role-only deltas)
pub fn parse_sse_line(line: &str) -> Option<SseEvent> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let payload = line.strip_prefix("data: ")?;
    if payload == "[DONE]" {
        return Some(SseEvent::Done);
    }
    let json: serde_json::Value = serde_json::from_str(payload).ok()?;
    let choices = json.get("choices")?.get(0)?;
    let delta = choices.get("delta")?;

    if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
        return Some(SseEvent::Content(content.to_string()));
    }

    // Reasoning models (o-series / gpt-5) stream `reasoning_content` deltas.
    if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
        if !reasoning.is_empty() {
            return Some(SseEvent::Reasoning(reasoning.to_string()));
        }
    }

    if delta.get("tool_calls").is_some() {
        return None;
    }

    None
}

/// Extract tool call data from an SSE line.
///
/// Returns `Some((index, id, name, arguments_fragment))` when the line
/// contains a tool_calls delta. `id` and `name` are only present in the
/// first chunk for a given index; subsequent chunks only have arguments
/// fragments.
pub fn extract_tool_call_delta(
    line: &str,
) -> Option<(usize, Option<String>, Option<String>, String)> {
    let line = line.trim();
    let payload = line.strip_prefix("data: ")?;
    if payload == "[DONE]" {
        return None;
    }
    let json: serde_json::Value = serde_json::from_str(payload).ok()?;
    let tool_calls = json
        .get("choices")?
        .get(0)?
        .get("delta")?
        .get("tool_calls")?
        .as_array()?;
    let tc = tool_calls.first()?;
    let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let id = tc.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
    let func = tc.get("function")?;
    let name = func
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let arguments = func
        .get("arguments")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some((index, id, name, arguments))
}

/// Check if a line has `finish_reason: "tool_calls"`.
pub fn has_tool_calls_finish(line: &str) -> bool {
    let line = line.trim();
    let Some(payload) = line.strip_prefix("data: ") else {
        return false;
    };
    if payload == "[DONE]" {
        return false;
    }
    let Ok(json) = serde_json::from_str::<serde_json::Value>(payload) else {
        return false;
    };
    json.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("finish_reason"))
        .and_then(|v| v.as_str())
        == Some("tool_calls")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic_content() {
        let line = r#"data: {"choices":[{"delta":{"content":"hello"}}]}"#;
        assert_eq!(
            parse_sse_line(line),
            Some(SseEvent::Content("hello".to_string()))
        );
    }

    #[test]
    fn test_parse_reasoning_content_delta() {
        let line = r#"data: {"choices":[{"delta":{"reasoning_content":"think"}}]}"#;
        assert_eq!(
            parse_sse_line(line),
            Some(SseEvent::Reasoning("think".to_string()))
        );
    }

    #[test]
    fn test_parse_done_marker() {
        let line = "data: [DONE]";
        assert_eq!(parse_sse_line(line), Some(SseEvent::Done));
    }

    #[test]
    fn test_parse_empty_line() {
        assert_eq!(parse_sse_line(""), None);
        assert_eq!(parse_sse_line("   "), None);
    }

    #[test]
    fn test_parse_no_data_prefix() {
        assert_eq!(parse_sse_line(": comment"), None);
        assert_eq!(parse_sse_line("event: ping"), None);
    }

    #[test]
    fn test_parse_no_content_field() {
        let line = r#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#;
        assert_eq!(parse_sse_line(line), None);
    }

    #[test]
    fn test_parse_multi_token() {
        let line = r#"data: {"choices":[{"delta":{"content":" world"}}]}"#;
        assert_eq!(
            parse_sse_line(line),
            Some(SseEvent::Content(" world".to_string()))
        );
    }

    #[test]
    fn test_parse_empty_content() {
        let line = r#"data: {"choices":[{"delta":{"content":""}}]}"#;
        assert_eq!(
            parse_sse_line(line),
            Some(SseEvent::Content("".to_string()))
        );
    }

    #[test]
    fn test_parse_trailing_whitespace() {
        let line = "data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}  \n";
        assert_eq!(
            parse_sse_line(line),
            Some(SseEvent::Content("x".to_string()))
        );
    }

    // --- Tool call tests ---

    #[test]
    fn test_parse_tool_calls_returns_none() {
        let line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_abc","function":{"name":"read_file","arguments":""}}]}}]}"#;
        assert_eq!(parse_sse_line(line), None);
    }

    #[test]
    fn test_extract_tool_call_first_chunk() {
        let line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_abc","function":{"name":"read_file","arguments":""}}]}}]}"#;
        let (index, id, name, args) = extract_tool_call_delta(line).unwrap();
        assert_eq!(index, 0);
        assert_eq!(id, Some("call_abc".to_string()));
        assert_eq!(name, Some("read_file".to_string()));
        assert_eq!(args, "");
    }

    #[test]
    fn test_extract_tool_call_args_chunk() {
        let line = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"pa"}}]}}]}"#;
        let (index, id, name, args) = extract_tool_call_delta(line).unwrap();
        assert_eq!(index, 0);
        assert_eq!(id, None);
        assert_eq!(name, None);
        assert_eq!(args, r#"{"pa"#);
    }

    #[test]
    fn test_has_tool_calls_finish() {
        let line = r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#;
        assert!(has_tool_calls_finish(line));
    }

    #[test]
    fn test_has_tool_calls_finish_false() {
        let line = r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        assert!(!has_tool_calls_finish(line));
        assert!(!has_tool_calls_finish("data: [DONE]"));
    }
}
