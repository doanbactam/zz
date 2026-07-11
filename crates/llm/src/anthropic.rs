//! Anthropic Messages API provider for ZeroZero .
//!
//! Native implementation of Anthropic's streaming Messages API.
//! Converts Anthropic SSE events (message_start, content_block_delta,
//! message_stop) to ZeroZero's `SseEvent` (Content, ToolCall, Done).
//!
//! Key differences from OpenAI:
//! - Endpoint: `/v1/messages` (not `/v1/chat/completions`)
//! - SSE uses `event:` + `data:` lines (OpenAI: `data:` only)
//! - Text delta: `content_block_delta` with `text_delta` (OpenAI: `choices[0].delta.content`)
//! - Tool streaming: `input_json_delta` with `partial_json` (OpenAI: `tool_calls` array)
//! - Stream end: `message_stop` event (OpenAI: `data: [DONE]`)
//! - System prompt: top-level `system` field (OpenAI: message role "system")
//! - Tool schema: `input_schema` (OpenAI: `function.parameters`)
//! - `max_tokens` required (OpenAI: optional)

use crate::{ChatMessage, Effort, Provider, SseEvent, SseEventStream};
use futures::{Stream, StreamExt};
use std::collections::{HashMap, VecDeque};
use std::pin::Pin;

// Parse a single SSE field line into `(field, value)` per the HTML5 SSE
// spec.
// Rules (per spec):
// - Lines starting with `:` are comments → `None`.
// - If the line contains a `:`, the field name is everything before the
//   first colon and the value is everything after it, with **exactly one**
//   leading space stripped (if present). `data:foo` and `data: foo` both
//   yield value `"foo"`; `data:  foo` yields `" foo"` (only one space
//   consumed).
// - If the line has no colon, the whole line is the field name with an
//   empty value.
// This is more lenient than the previous `strip_prefix("data: ")` approach,
// which silently dropped events whose `data:` field had no trailing space
// (a valid SSE encoding).
fn parse_sse_field(line: &str) -> Option<(&str, &str)> {
    if line.starts_with(':') {
        return None; // comment line
    }
    let (field, value) = match line.find(':') {
        Some(idx) => (&line[..idx], &line[idx + 1..]),
        None => (line, ""),
    };
    // Strip exactly one leading space from the value per the SSE spec.
    let value = value.strip_prefix(' ').unwrap_or(value);
    Some((field, value))
}

// Anthropic Messages API provider with SSE streaming.
#[derive(Clone)]
pub struct AnthropicProvider {
    api_key: String,
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        Self {
            api_key,
            base_url,
            model,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    // messages format. Extracts system messages to a top-level system
    // prompt string.
    // `images` : when non-empty, attached to the **last user
    // message** as Anthropic image content blocks
    // `{type:"image", source:{type:"base64", media_type, data}}`. The
    // `media_type` is parsed from the image data-URL prefix
    // (`data:image/png;base64,...` → `image/png`); unknown prefixes
    // default to `image/png`.
    // Returns an error if an assistant message carries a tool_call whose
    // `arguments` field is not valid JSON — Anthropic requires `input` to
    // be a JSON object, so silently substituting `null` would send a
    // malformed request.
    fn convert_messages(
        messages: &[ChatMessage],
        images: &[String],
    ) -> anyhow::Result<(Option<String>, Vec<serde_json::Value>)> {
        let mut system_prompt: Option<String> = None;
        let mut anthropic_msgs: Vec<serde_json::Value> = Vec::new();

        for (idx, msg) in messages.iter().enumerate() {
            let is_last_user = idx == messages.len() - 1 && msg.role == "user";
            match msg.role.as_str() {
                "system" => {
                    // Anthropic: system is top-level field, not a message.
                    if system_prompt.is_none() {
                        system_prompt = Some(msg.content.clone());
                    } else {
                        // Append if multiple system messages.
                        if let Some(s) = &mut system_prompt {
                            s.push('\n');
                            s.push_str(&msg.content);
                        }
                    }
                }
                "user" | "assistant" => {
                    let mut entry = serde_json::json!({
                        "role": msg.role,
                        "content": msg.content,
                    });
                    // If assistant has tool_calls, convert to content blocks.
                    if let Some(tool_calls) = &msg.tool_calls {
                        let mut content: Vec<serde_json::Value> = Vec::new();
                        // Only emit a text block when there is actual text.
                        // Anthropic rejects empty text blocks in many cases,
                        // and an empty block is semantically meaningless when
                        // the assistant only produced tool calls.
                        if !msg.content.is_empty() {
                            content.push(serde_json::json!({
                                "type": "text",
                                "text": msg.content,
                            }));
                        }
                        for tc in tool_calls {
                            let input: serde_json::Value =
                                serde_json::from_str(&tc.function.arguments).map_err(|e| {
                                    anyhow::anyhow!(
                                        "assistant tool_call {} has invalid JSON arguments: {e}",
                                        tc.id
                                    )
                                })?;
                            content.push(serde_json::json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.function.name,
                                "input": input,
                            }));
                        }
                        // Guard: a tool_use-only assistant message must have
                        // at least one tool_use block. If tool_calls was Some
                        // but empty (shouldn't happen in practice), fall back
                        // to the original string content to avoid an empty
                        // content array, which Anthropic rejects.
                        if content.is_empty() {
                            content.push(serde_json::json!({
                                "type": "text",
                                "text": msg.content,
                            }));
                        }
                        entry["content"] = serde_json::Value::Array(content);
                    }

                    // Extended thinking round-trip: when an assistant message
                    // carries thinking content + signature (or redacted
                    // thinking), prepend thinking blocks to the content array.
                    // Anthropic requires these blocks to be sent back verbatim
                    // in multi-turn conversations with extended thinking.
                    // Block ordering: thinking → redacted_thinking → text → tool_use.
                    if msg.role == "assistant"
                        && (msg.thinking.is_some()
                            || msg.thinking_signature.is_some()
                            || msg.redacted_thinking.is_some())
                    {
                        let mut content: Vec<serde_json::Value> = match entry["content"].take() {
                            serde_json::Value::Array(arr) => arr,
                            serde_json::Value::String(s) => {
                                if s.is_empty() {
                                    Vec::new()
                                } else {
                                    vec![serde_json::json!({
                                        "type": "text",
                                        "text": s,
                                    })]
                                }
                            }
                            _ => Vec::new(),
                        };
                        // Prepend thinking blocks (insert at index 0, in
                        // reverse order so the final order is correct).
                        if let Some(rt) = &msg.redacted_thinking {
                            content.insert(
                                0,
                                serde_json::json!({
                                    "type": "redacted_thinking",
                                    "data": rt,
                                }),
                            );
                        }
                        if let (Some(thinking), Some(sig)) =
                            (&msg.thinking, &msg.thinking_signature)
                        {
                            content.insert(
                                0,
                                serde_json::json!({
                                    "type": "thinking",
                                    "thinking": thinking,
                                    "signature": sig,
                                }),
                            );
                        }
                        if !content.is_empty() {
                            entry["content"] = serde_json::Value::Array(content);
                        }
                    }

                    // attach images to the last user message.
                    if is_last_user && !images.is_empty() {
                        let mut content: Vec<serde_json::Value> = match entry["content"].take() {
                            serde_json::Value::Array(arr) => arr,
                            serde_json::Value::String(s) => {
                                if s.is_empty() {
                                    Vec::new()
                                } else {
                                    vec![serde_json::json!({
                                        "type": "text",
                                        "text": s,
                                    })]
                                }
                            }
                            _ => Vec::new(),
                        };
                        for img in images {
                            let (media_type, data) = Self::parse_data_url(img);
                            content.push(serde_json::json!({
                                "type": "image",
                                "source": {
                                    "type": "base64",
                                    "media_type": media_type,
                                    "data": data,
                                }
                            }));
                        }
                        entry["content"] = serde_json::Value::Array(content);
                    }
                    anthropic_msgs.push(entry);
                }
                "tool" => {
                    // OpenAI tool result message → Anthropic user message
                    // with tool_result content block.
                    let tool_call_id = msg.tool_call_id.clone().unwrap_or_default();
                    let entry = serde_json::json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": tool_call_id,
                            "content": msg.content,
                        }],
                    });
                    anthropic_msgs.push(entry);
                }
                _ => {
                    // Unknown role — pass through as-is.
                    anthropic_msgs.push(serde_json::json!({
                        "role": msg.role,
                        "content": msg.content,
                    }));
                }
            }
        }

        Ok((system_prompt, anthropic_msgs))
    }

    // Parse a `data:<mime>;base64,<data>` URL into (mime, base64 data).
    // Returns `("image/png", <data>)` when the URL doesn't match the
    // expected prefix (defensive default for malformed input).
    fn parse_data_url(url: &str) -> (&str, &str) {
        // Expected: "data:image/png;base64,XXXX"
        let rest = match url.strip_prefix("data:") {
            Some(r) => r,
            None => return ("image/png", url),
        };
        // Split mime (before first comma) from the rest.
        let (mime_and_enc, data) = match rest.find(',') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => return ("image/png", rest),
        };
        // mime_and_enc looks like "image/png;base64". Strip the encoding.
        let media_type = mime_and_enc.split(';').next().unwrap_or("image/png").trim();
        if media_type.is_empty() {
            ("image/png", data)
        } else {
            (media_type, data)
        }
    }

    // Convert OpenAI tool definitions to Anthropic format.
    // OpenAI: {"type":"function","function":{"name":"bash","description":"...","parameters":{...}}}
    // Anthropic: {"name":"bash","description":"...","input_schema":{...}}
    // Returns an error if a tool definition is missing `name` (required by
    // Anthropic). `description` is optional per the Anthropic spec, so a
    // missing description is left as `null` rather than erroring.
    fn convert_tools(tools: &[serde_json::Value]) -> anyhow::Result<Vec<serde_json::Value>> {
        tools
            .iter()
            .map(|t| {
                let func = t.get("function").unwrap_or(t);
                let name = func.get("name").and_then(|v| v.as_str()).ok_or_else(|| {
                    anyhow::anyhow!("tool definition missing required `name` field: {t}")
                })?;
                let description = func.get("description").cloned().unwrap_or_default();
                let input_schema = func
                    .get("parameters")
                    .or_else(|| func.get("input_schema"))
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
                Ok(serde_json::json!({
                    "name": name,
                    "description": description,
                    "input_schema": input_schema,
                }))
            })
            .collect()
    }
}

#[async_trait::async_trait]
impl Provider for AnthropicProvider {
    async fn chat_stream(
        &self,
        prompt: &str,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<String>> + Send>>> {
        let messages = vec![ChatMessage {
            role: "user".to_string(),
            content: prompt.to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        }];
        let event_stream = self
            .chat_with_tools(&messages, &[], Effort::None, &[])
            .await?;
        let content_stream = event_stream.filter_map(|item| async move {
            match item {
                Ok(SseEvent::Content(text)) => Some(Ok(text)),
                Ok(SseEvent::Reasoning(_)) => None,
                Ok(SseEvent::ThinkingBlock { .. }) => None,
                Ok(SseEvent::RedactedThinking(_)) => None,
                Ok(SseEvent::ToolCall { .. }) => None,
                Ok(SseEvent::Done) => None,
                Err(e) => Some(Err(e)),
            }
        });
        Ok(Box::pin(content_stream))
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        effort: Effort,
        images: &[String],
    ) -> anyhow::Result<SseEventStream> {
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let (system, anthropic_msgs) = Self::convert_messages(messages, images)?;
        let anthropic_tools = Self::convert_tools(tools)?;

        //B fix: max_tokens is dynamic when thinking is enabled.
        // budget_tokens must be < max_tokens (strictly). When effort != None,
        // raise max_tokens to budget + 4096. When effort == None, preserve
        // the prior hardcoded 4096 (no regression).
        let budget = effort.budget_tokens();
        let max_tokens = budget.map(|b| b + 4096).unwrap_or(4096);

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "messages": anthropic_msgs,
            "stream": true,
        });
        // Extended thinking: only send when effort != None (budget present).
        if let Some(b) = budget {
            body["thinking"] = serde_json::json!({
                "type": "enabled",
                "budget_tokens": b,
            });
        }
        if let Some(sys) = system {
            body["system"] = serde_json::Value::String(sys);
        }
        if !anthropic_tools.is_empty() {
            body["tools"] = serde_json::Value::Array(anthropic_tools);
        }

        let response = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        //E: If the model rejects thinking with HTTP 400 and effort was
        // set, retry once without it (Effort::None) to recover.
        if response.status().as_u16() == 400 && effort != Effort::None {
            return self
                .chat_with_tools(messages, tools, Effort::None, images)
                .await;
        }

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Anthropic HTTP error {status}: {text}");
        }

        let byte_stream = response.bytes_stream();
        let stream = AnthropicSseStream::new(byte_stream);
        Ok(Box::pin(stream))
    }

    fn model(&self) -> &str {
        &self.model
    }
}

// A tool use block being accumulated during streaming.
struct ToolBlock {
    id: String,
    name: String,
    input_buffer: String,
}

// A thinking block being accumulated during streaming (extended thinking).
// Captures both the thinking text and the cryptographic signature that
// Anthropic requires to be sent back verbatim in multi-turn conversations.
struct ThinkingBlockState {
    thinking: String,
    signature: String,
}

// Adapter that converts Anthropic SSE byte stream into `SseEvent` stream.
// Anthropic SSE format:
// ```text
// event: content_block_delta
// data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}
// ```
// We parse `event:` + `data:` pairs, then dispatch based on event type.
struct AnthropicSseStream<S> {
    inner: S,
    buffer: String,
    // Per-index tool use blocks being accumulated.
    tool_blocks: HashMap<usize, ToolBlock>,
    // Per-index thinking blocks being accumulated (extended thinking).
    thinking_blocks: HashMap<usize, ThinkingBlockState>,
    // Per-index redacted thinking data (safety-flagged, encrypted).
    // Captured at content_block_start and emitted at content_block_stop.
    redacted_blocks: HashMap<usize, String>,
    // Pending events queued for emission (pending queue pattern).
    pending: VecDeque<anyhow::Result<SseEvent>>,
}

impl<S> AnthropicSseStream<S>
where
    S: Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin + Send,
{
    fn new(inner: S) -> Self {
        Self {
            inner,
            buffer: String::new(),
            tool_blocks: HashMap::new(),
            thinking_blocks: HashMap::new(),
            redacted_blocks: HashMap::new(),
            pending: VecDeque::new(),
        }
    }

    // Process a complete SSE event (event: line + data: line) and queue
    // any resulting SseEvents into `pending`.
    // `data_value` is the already-parsed `data:` field value (the JSON
    // payload), with the leading `data:` field name and the single
    // optional space stripped per the SSE spec.
    fn process_event(&mut self, event_type: &str, data_value: &str) {
        let payload = data_value.trim();

        match event_type {
            "content_block_start" => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(payload) {
                    if let Some(block) = json.get("content_block") {
                        let index =
                            json.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                        let block_type = block.get("type").and_then(|t| t.as_str());
                        match block_type {
                            Some("tool_use") => {
                                let id = block
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let name = block
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                self.tool_blocks.insert(
                                    index,
                                    ToolBlock {
                                        id,
                                        name,
                                        input_buffer: String::new(),
                                    },
                                );
                            }
                            Some("thinking") => {
                                self.thinking_blocks.insert(
                                    index,
                                    ThinkingBlockState {
                                        thinking: String::new(),
                                        signature: String::new(),
                                    },
                                );
                            }
                            Some("redacted_thinking") => {
                                let data = block
                                    .get("data")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                self.redacted_blocks.insert(index, data);
                            }
                            _ => {}
                        }
                    }
                }
            }
            "content_block_delta" => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(payload) {
                    let index = json.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                    if let Some(delta) = json.get("delta") {
                        let delta_type = delta.get("type").and_then(|t| t.as_str());
                        match delta_type {
                            Some("text_delta") => {
                                if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                    self.pending
                                        .push_back(Ok(SseEvent::Content(text.to_string())));
                                }
                            }
                            Some("thinking_delta") => {
                                if let Some(text) = delta.get("thinking").and_then(|t| t.as_str()) {
                                    // Emit live reasoning for TUI display.
                                    self.pending
                                        .push_back(Ok(SseEvent::Reasoning(text.to_string())));
                                    // Accumulate into the thinking block for
                                    // round-trip signature capture.
                                    if let Some(block) = self.thinking_blocks.get_mut(&index) {
                                        block.thinking.push_str(text);
                                    }
                                }
                            }
                            Some("signature_delta") => {
                                // Cryptographic signature for the thinking
                                // block at this index. Accumulate — usually
                                // a single chunk but may be split.
                                if let Some(sig) = delta.get("signature").and_then(|s| s.as_str()) {
                                    if let Some(block) = self.thinking_blocks.get_mut(&index) {
                                        block.signature.push_str(sig);
                                    }
                                }
                            }
                            Some("input_json_delta") => {
                                if let Some(partial) =
                                    delta.get("partial_json").and_then(|p| p.as_str())
                                {
                                    if let Some(block) = self.tool_blocks.get_mut(&index) {
                                        block.input_buffer.push_str(partial);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            "content_block_stop" => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(payload) {
                    let index = json.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                    // Tool use block complete → emit ToolCall.
                    if let Some(block) = self.tool_blocks.remove(&index) {
                        self.pending.push_back(Ok(SseEvent::ToolCall {
                            id: block.id,
                            name: block.name,
                            arguments: block.input_buffer,
                        }));
                    }
                    // Thinking block complete → emit ThinkingBlock with
                    // the accumulated text + signature for round-trip.
                    if let Some(block) = self.thinking_blocks.remove(&index) {
                        self.pending.push_back(Ok(SseEvent::ThinkingBlock {
                            thinking: block.thinking,
                            signature: block.signature,
                        }));
                    }
                    // Redacted thinking block complete → emit data.
                    if let Some(data) = self.redacted_blocks.remove(&index) {
                        self.pending.push_back(Ok(SseEvent::RedactedThinking(data)));
                    }
                }
            }
            "message_stop" => {
                self.pending.push_back(Ok(SseEvent::Done));
            }
            _ => {
                // message_start, message_delta, ping, error — ignore for now.
            }
        }
    }

    // Extract complete SSE events from buffer. Returns true if any were
    // processed (so caller can continue loop).
    fn try_extract_events(&mut self) -> bool {
        let mut processed = false;
        // SSE events are separated by double newline (\n\n).
        // Each event has `event: <type>` and `data: <payload>` lines.
        while let Some(pos) = self.buffer.find("\n\n") {
            let chunk = self.buffer[..pos].to_string();
            self.buffer = self.buffer[pos + 2..].to_string();

            // Parse event: and data: lines from chunk using the
            // spec-compliant field parser (handles `data:foo` with no
            // space, `data:  foo` with two spaces, etc.).
            let mut event_type: Option<String> = None;
            let mut data_value: Option<String> = None;
            for line in chunk.lines() {
                if let Some((field, value)) = parse_sse_field(line) {
                    match field {
                        "event" => event_type = Some(value.trim().to_string()),
                        "data" => {
                            // Anthropic sends a single data: line per event.
                            // If multiple appear, concatenate with newline
                            // per the SSE spec.
                            match &mut data_value {
                                Some(existing) => {
                                    existing.push('\n');
                                    existing.push_str(value);
                                }
                                None => data_value = Some(value.to_string()),
                            }
                        }
                        _ => {}
                    }
                }
            }

            if let (Some(et), Some(data)) = (event_type, &data_value) {
                self.process_event(&et, data);
                processed = true;
            }
        }
        processed
    }
}

impl<S> Stream for AnthropicSseStream<S>
where
    S: Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin + Send,
{
    type Item = anyhow::Result<SseEvent>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();

        loop {
            // 1. Pop from pending queue first.
            if let Some(event) = this.pending.pop_front() {
                return std::task::Poll::Ready(Some(event));
            }

            // 2. Try to extract complete SSE events from buffer.
            if this.try_extract_events() {
                continue;
            }

            // 3. Need more data from inner stream.
            use futures::StreamExt;
            match this.inner.poll_next_unpin(cx) {
                std::task::Poll::Ready(Some(chunk_result)) => match chunk_result {
                    Ok(chunk) => {
                        this.buffer.push_str(&String::from_utf8_lossy(&chunk));
                        continue;
                    }
                    Err(e) => {
                        return std::task::Poll::Ready(Some(Err(anyhow::anyhow!(e))));
                    }
                },
                std::task::Poll::Ready(None) => {
                    // Stream ended — process any remaining buffer.
                    if !this.buffer.is_empty() {
                        let remaining = this.buffer.clone();
                        this.buffer.clear();
                        // Try to parse as a final event (may not have \n\n).
                        let mut event_type: Option<String> = None;
                        let mut data_value: Option<String> = None;
                        for line in remaining.lines() {
                            if let Some((field, value)) = parse_sse_field(line) {
                                match field {
                                    "event" => event_type = Some(value.trim().to_string()),
                                    "data" => match &mut data_value {
                                        Some(existing) => {
                                            existing.push('\n');
                                            existing.push_str(value);
                                        }
                                        None => data_value = Some(value.to_string()),
                                    },
                                    _ => {}
                                }
                            }
                        }
                        if let (Some(et), Some(data)) = (event_type, &data_value) {
                            this.process_event(&et, data);
                        }
                    }
                    if this.pending.is_empty() {
                        return std::task::Poll::Ready(None);
                    }
                    continue;
                }
                std::task::Poll::Pending => {
                    return std::task::Poll::Pending;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ToolCall, ToolCallFunction};
    use futures::stream;

    // Helper: build a fake bytes-stream from string chunks.
    fn make_bytes_stream(
        chunks: Vec<&str>,
    ) -> impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> {
        stream::iter(
            chunks
                .into_iter()
                .map(|s| Ok(bytes::Bytes::from(s.to_string()))),
        )
    }

    #[test]
    fn test_prd11_ac1_provider_construction() {
        let p = AnthropicProvider::new(
            "sk-ant-test".to_string(),
            "https://api.anthropic.com".to_string(),
            "claude-sonnet-4-20250514".to_string(),
        );
        assert_eq!(p.api_key, "sk-ant-test");
        assert_eq!(p.base_url, "https://api.anthropic.com");
        assert_eq!(p.model, "claude-sonnet-4-20250514");
    }

    #[tokio::test]
    async fn test_thinking_delta_streams_as_reasoning() {
        let sse = concat!(
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let byte_stream = make_bytes_stream(vec![sse]);
        let mut stream = AnthropicSseStream::new(byte_stream);
        use futures::StreamExt as _;
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let mut out = Vec::new();
            while let Some(item) = stream.next().await {
                if let Ok(SseEvent::Reasoning(t)) = item {
                    out.push(t);
                }
            }
            out
        })
        .await
        .expect("stream must terminate");
        assert_eq!(result, vec!["hmm".to_string()]);
    }

    #[tokio::test]
    async fn test_prd11_ac2_text_streaming_content_events() {
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let byte_stream = make_bytes_stream(vec![sse]);
        let mut stream = AnthropicSseStream::new(byte_stream);

        use futures::StreamExt as _;
        let mut events: Vec<SseEvent> = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(e) => events.push(e),
                Err(_) => break,
            }
        }

        let contents: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                SseEvent::Content(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            contents,
            vec!["Hello".to_string(), " world".to_string()],
            "Both text deltas must emit. Got: {:?}",
            events
        );
        assert!(
            events.iter().any(|e| matches!(e, SseEvent::Done)),
            "Done must emit from message_stop. Got: {:?}",
            events
        );
    }

    #[tokio::test]
    async fn test_prd11_ac3_tool_use_streaming_toolcall_event() {
        let sse = concat!(
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_123\",\"name\":\"bash\",\"input\":{}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"cmd\\\":\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"ls\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let byte_stream = make_bytes_stream(vec![sse]);
        let mut stream = AnthropicSseStream::new(byte_stream);

        use futures::StreamExt as _;
        let mut events: Vec<SseEvent> = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(e) => events.push(e),
                Err(_) => break,
            }
        }

        let tool_call = events.iter().find_map(|e| match e {
            SseEvent::ToolCall {
                id,
                name,
                arguments,
            } => Some((id.clone(), name.clone(), arguments.clone())),
            _ => None,
        });
        let (id, name, args) = tool_call.expect("ToolCall must emit");
        assert_eq!(id, "toolu_123");
        assert_eq!(name, "bash");
        assert_eq!(args, r#"{"cmd":"ls"}"#);
        assert!(
            events.iter().any(|e| matches!(e, SseEvent::Done)),
            "Done must emit. Got: {:?}",
            events
        );
    }

    #[tokio::test]
    async fn test_prd11_ac4_message_stop_emits_done() {
        let sse = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let byte_stream = make_bytes_stream(vec![sse]);
        let mut stream = AnthropicSseStream::new(byte_stream);

        use futures::StreamExt as _;
        let mut events: Vec<SseEvent> = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(e) => events.push(e),
                Err(_) => break,
            }
        }

        assert_eq!(
            events.len(),
            1,
            "Only Done event expected. Got: {:?}",
            events
        );
        assert!(matches!(events[0], SseEvent::Done));
    }

    #[tokio::test]
    async fn test_prd11_ac5_multi_tool_call_all_emit() {
        let sse = concat!(
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t0\",\"name\":\"bash\",\"input\":{}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"grep\",\"input\":{}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let byte_stream = make_bytes_stream(vec![sse]);
        let mut stream = AnthropicSseStream::new(byte_stream);

        use futures::StreamExt as _;
        let mut events: Vec<SseEvent> = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(e) => events.push(e),
                Err(_) => break,
            }
        }

        let tool_calls: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                SseEvent::ToolCall { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            tool_calls,
            vec!["bash", "grep"],
            "Both tool calls must emit. Got: {:?}",
            events
        );
        assert_eq!(
            events.len(),
            3,
            "3 events: 2 ToolCall + 1 Done. Got: {:?}",
            events
        );
    }

    #[test]
    fn test_prd11_ac6_system_prompt_extraction() {
        let messages = vec![
            ChatMessage {
                role: "system".to_string(),
                content: "You are helpful".to_string(),
                tool_call_id: None,
                tool_calls: None,
                attachments: None,
                thinking_signature: None,
                redacted_thinking: None,
                thinking: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
                tool_call_id: None,
                tool_calls: None,
                attachments: None,
                thinking_signature: None,
                redacted_thinking: None,
                thinking: None,
            },
        ];
        let (system, anthropic_msgs) = AnthropicProvider::convert_messages(&messages, &[]).unwrap();
        assert_eq!(system, Some("You are helpful".to_string()));
        assert_eq!(
            anthropic_msgs.len(),
            1,
            "Only user message (system extracted)"
        );
        assert_eq!(anthropic_msgs[0]["role"], "user");
        assert_eq!(anthropic_msgs[0]["content"], "Hello");
    }

    #[test]
    fn test_prd11_ac7_tool_definition_conversion() {
        let openai_tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "bash",
                "description": "Run a bash command",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {"type": "string"}
                    },
                    "required": ["command"]
                }
            }
        })];
        let anthropic_tools = AnthropicProvider::convert_tools(&openai_tools).unwrap();
        assert_eq!(anthropic_tools.len(), 1);
        assert_eq!(anthropic_tools[0]["name"], "bash");
        assert_eq!(anthropic_tools[0]["description"], "Run a bash command");
        assert!(anthropic_tools[0]["input_schema"].is_object());
        assert_eq!(anthropic_tools[0]["input_schema"]["type"], "object");
    }

    #[test]
    fn test_tool_result_message_conversion() {
        let messages = vec![ChatMessage {
            role: "tool".to_string(),
            content: "command output".to_string(),
            tool_call_id: Some("call_123".to_string()),
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        }];
        let (_system, anthropic_msgs) =
            AnthropicProvider::convert_messages(&messages, &[]).unwrap();
        assert_eq!(anthropic_msgs.len(), 1);
        assert_eq!(anthropic_msgs[0]["role"], "user");
        let content = anthropic_msgs[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "call_123");
        assert_eq!(content[0]["content"], "command output");
    }

    #[test]
    fn test_assistant_with_tool_calls_conversion() {
        let messages = vec![ChatMessage {
            role: "assistant".to_string(),
            content: "Let me run that".to_string(),
            tool_call_id: None,
            tool_calls: Some(vec![crate::ToolCall {
                id: "call_456".to_string(),
                call_type: "function".to_string(),
                function: crate::ToolCallFunction {
                    name: "bash".to_string(),
                    arguments: r#"{"command":"ls"}"#.to_string(),
                },
            }]),
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        }];
        let (_system, anthropic_msgs) =
            AnthropicProvider::convert_messages(&messages, &[]).unwrap();
        assert_eq!(anthropic_msgs.len(), 1);
        assert_eq!(anthropic_msgs[0]["role"], "assistant");
        let content = anthropic_msgs[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2, "text block + tool_use block");
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["id"], "call_456");
        assert_eq!(content[1]["name"], "bash");
        assert_eq!(content[1]["input"]["command"], "ls");
    }

    // ---- Tests for review fixes  ----

    #[test]
    fn test_parse_sse_field_with_space() {
        assert_eq!(parse_sse_field("data: hello"), Some(("data", "hello")));
        assert_eq!(parse_sse_field("event: ping"), Some(("event", "ping")));
    }

    #[test]
    fn test_parse_sse_field_without_space() {
        // `data:foo` (no space) is valid SSE — value is "foo".
        assert_eq!(parse_sse_field("data:foo"), Some(("data", "foo")));
        assert_eq!(parse_sse_field("event:ping"), Some(("event", "ping")));
    }

    #[test]
    fn test_parse_sse_field_two_spaces_only_one_stripped() {
        // Per spec, only ONE leading space is stripped. `data:  foo` → " foo".
        assert_eq!(parse_sse_field("data:  foo"), Some(("data", " foo")));
    }

    #[test]
    fn test_parse_sse_field_comment_line() {
        // Lines starting with `:` are comments.
        assert_eq!(parse_sse_field(": this is a comment"), None);
    }

    #[test]
    fn test_parse_sse_field_no_colon() {
        // A line with no colon is a field with empty value.
        assert_eq!(parse_sse_field("retry"), Some(("retry", "")));
    }

    #[test]
    fn test_parse_sse_field_empty_data() {
        assert_eq!(parse_sse_field("data:"), Some(("data", "")));
        assert_eq!(parse_sse_field("data: "), Some(("data", "")));
    }

    // Fix #1: assistant message with empty content + tool_calls must NOT
    // emit an empty text block (Anthropic rejects empty text blocks).
    #[test]
    fn test_assistant_tool_only_no_empty_text_block() {
        let messages = vec![ChatMessage {
            role: "assistant".to_string(),
            content: "".to_string(),
            tool_call_id: None,
            tool_calls: Some(vec![crate::ToolCall {
                id: "call_789".to_string(),
                call_type: "function".to_string(),
                function: crate::ToolCallFunction {
                    name: "bash".to_string(),
                    arguments: r#"{"command":"ls"}"#.to_string(),
                },
            }]),
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        }];
        let (_system, anthropic_msgs) =
            AnthropicProvider::convert_messages(&messages, &[]).unwrap();
        assert_eq!(anthropic_msgs.len(), 1);
        let content = anthropic_msgs[0]["content"].as_array().unwrap();
        // Only the tool_use block — no empty text block.
        assert_eq!(
            content.len(),
            1,
            "No empty text block when content is empty. Got: {:?}",
            content
        );
        assert_eq!(content[0]["type"], "tool_use");
        assert_eq!(content[0]["name"], "bash");
    }

    // Fix #1: assistant message with non-empty content + tool_calls emits
    // both text and tool_use blocks.
    #[test]
    fn test_assistant_with_text_and_tool_calls() {
        let messages = vec![ChatMessage {
            role: "assistant".to_string(),
            content: "Running it now".to_string(),
            tool_call_id: None,
            tool_calls: Some(vec![crate::ToolCall {
                id: "call_000".to_string(),
                call_type: "function".to_string(),
                function: crate::ToolCallFunction {
                    name: "bash".to_string(),
                    arguments: r#"{"command":"pwd"}"#.to_string(),
                },
            }]),
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        }];
        let (_system, anthropic_msgs) =
            AnthropicProvider::convert_messages(&messages, &[]).unwrap();
        let content = anthropic_msgs[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Running it now");
        assert_eq!(content[1]["type"], "tool_use");
    }

    // Fix #5: convert_messages errors on invalid JSON tool_call arguments
    // instead of silently substituting null.
    #[test]
    fn test_convert_messages_errors_on_invalid_arguments() {
        let messages = vec![ChatMessage {
            role: "assistant".to_string(),
            content: "".to_string(),
            tool_call_id: None,
            tool_calls: Some(vec![crate::ToolCall {
                id: "call_bad".to_string(),
                call_type: "function".to_string(),
                function: crate::ToolCallFunction {
                    name: "bash".to_string(),
                    arguments: "not valid json".to_string(),
                },
            }]),
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        }];
        let result = AnthropicProvider::convert_messages(&messages, &[]);
        assert!(result.is_err(), "Should error on invalid JSON arguments");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("call_bad"),
            "Error should mention the tool call id. Got: {err}"
        );
    }

    // Fix #4: convert_tools errors when a tool definition is missing `name`.
    #[test]
    fn test_convert_tools_errors_on_missing_name() {
        let bad_tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "description": "no name here",
                "parameters": {"type": "object"}
            }
        })];
        let result = AnthropicProvider::convert_tools(&bad_tools);
        assert!(result.is_err(), "Should error on missing name");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("name"),
            "Error should mention the missing name field. Got: {err}"
        );
    }

    // Fix #4: convert_tools accepts a tool with no description (optional
    // per Anthropic spec) — should NOT error, description becomes null.
    #[test]
    fn test_convert_tools_no_description_ok() {
        let tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "grep",
                "parameters": {"type": "object"}
            }
        })];
        let anthropic_tools = AnthropicProvider::convert_tools(&tools).unwrap();
        assert_eq!(anthropic_tools.len(), 1);
        assert_eq!(anthropic_tools[0]["name"], "grep");
        assert!(anthropic_tools[0]["description"].is_null());
    }

    // Fix #2: SSE stream with `data:` (no trailing space) must still parse.
    // This is valid per the SSE spec and was previously dropped silently.
    #[tokio::test]
    async fn test_sse_data_without_space_still_parses() {
        let sse = concat!(
            "event: message_start\n",
            "data:{\"type\":\"message_start\",\"message\":{}}\n\n",
            "event: content_block_delta\n",
            "data:{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"nospace\"}}\n\n",
            "event: message_stop\n",
            "data:{\"type\":\"message_stop\"}\n\n",
        );
        let byte_stream = make_bytes_stream(vec![sse]);
        let mut stream = AnthropicSseStream::new(byte_stream);

        use futures::StreamExt as _;
        let mut events: Vec<SseEvent> = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(e) => events.push(e),
                Err(_) => break,
            }
        }

        let has_content = events.iter().any(|e| match e {
            SseEvent::Content(s) => s == "nospace",
            _ => false,
        });
        assert!(
            has_content,
            "Content from data: (no space) must parse. Got: {:?}",
            events
        );
        assert!(
            events.iter().any(|e| matches!(e, SseEvent::Done)),
            "Done must emit. Got: {:?}",
            events
        );
    }

    // Fix #2: SSE comment lines (starting with `:`) must be ignored,
    // not treated as data.
    #[tokio::test]
    async fn test_sse_comment_lines_ignored() {
        let sse = concat!(
            ": this is a keep-alive comment\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let byte_stream = make_bytes_stream(vec![sse]);
        let mut stream = AnthropicSseStream::new(byte_stream);

        use futures::StreamExt as _;
        let mut events: Vec<SseEvent> = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(e) => events.push(e),
                Err(_) => break,
            }
        }

        // Exactly one Content + one Done. The comment line must not produce
        // a phantom event.
        let content_count = events
            .iter()
            .filter(|e| matches!(e, SseEvent::Content(_)))
            .count();
        assert_eq!(content_count, 1, "Comment line must not emit content");
        assert!(events.iter().any(|e| matches!(e, SseEvent::Done)));
    }

    // --- : Anthropic thinking + budget + retry tests ---
    // Written by B (Test Author, Round 7) based on + design only.

    // AC-3: effort=High → body contains thinking.budget_tokens=16000,
    // max_tokens=16000+4096=20096.B: max_tokens = budget + 4096.
    #[tokio::test]
    async fn test_anthropic_thinking_budget() {
        let server = wiremock::MockServer::start().await;
        let sse = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/messages"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/event-stream")
                    .set_body_raw(sse.as_bytes(), "text/event-stream"),
            )
            .mount(&server)
            .await;

        let provider = AnthropicProvider::new(
            "sk-ant-test".to_string(),
            server.uri(),
            "claude-opus-4".to_string(),
        );
        let msgs = vec![ChatMessage {
            role: "user".to_string(),
            content: "test".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        }];
        let _ = provider
            .chat_with_tools(&msgs, &[], Effort::High, &[])
            .await
            .expect("chat_with_tools succeeds");

        let requests = server.received_requests().await.expect("requests captured");
        assert!(!requests.is_empty());
        let body: serde_json::Value =
            serde_json::from_slice(&requests[0].body).expect("body is valid JSON");
        // thinking object with budget_tokens.
        assert_eq!(
            body["thinking"]["type"].as_str(),
            Some("enabled"),
            "thinking.type must be 'enabled'"
        );
        assert_eq!(
            body["thinking"]["budget_tokens"].as_u64(),
            Some(16000),
            "thinking.budget_tokens must be 16000 for High"
        );
        //B: max_tokens = budget + 4096 = 20096.
        assert_eq!(
            body["max_tokens"].as_u64(),
            Some(20096),
            "max_tokens must be budget(16000) + 4096 = 20096. Got: {}",
            body["max_tokens"]
        );
        // Verify budget < max_tokens (Anthropic strict rule).
        let budget = body["thinking"]["budget_tokens"].as_u64().unwrap();
        let max = body["max_tokens"].as_u64().unwrap();
        assert!(
            budget < max,
            "budget_tokens ({budget}) must be < max_tokens ({max})"
        );
    }

    // AC-3: effort=None → body must NOT contain thinking, max_tokens=4096.
    #[tokio::test]
    async fn test_anthropic_no_thinking_when_none() {
        let server = wiremock::MockServer::start().await;
        let sse = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/messages"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/event-stream")
                    .set_body_raw(sse.as_bytes(), "text/event-stream"),
            )
            .mount(&server)
            .await;

        let provider = AnthropicProvider::new(
            "sk-ant-test".to_string(),
            server.uri(),
            "claude-opus-4".to_string(),
        );
        let msgs = vec![ChatMessage {
            role: "user".to_string(),
            content: "test".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        }];
        let _ = provider
            .chat_with_tools(&msgs, &[], Effort::None, &[])
            .await
            .expect("chat_with_tools succeeds");

        let requests = server.received_requests().await.expect("requests captured");
        let body: serde_json::Value =
            serde_json::from_slice(&requests[0].body).expect("body is valid JSON");
        assert!(
            body.get("thinking").is_none(),
            "thinking must NOT be present when effort is None. Got: {body}"
        );
        assert_eq!(
            body["max_tokens"].as_u64(),
            Some(4096),
            "max_tokens must be 4096 when effort is None"
        );
    }

    // AC-5: When Anthropic returns 400 for thinking, retry without
    // thinking. Verify two requests: first with thinking, second without.
    #[tokio::test]
    async fn test_anthropic_400_retry() {
        let server = wiremock::MockServer::start().await;

        // First request (with thinking) → 400.
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/messages"))
            .and(wiremock::matchers::body_string_contains("thinking"))
            .respond_with(wiremock::ResponseTemplate::new(400).set_body_string(
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"thinking is not supported for this model"}}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        // Second request (without thinking) → 200 SSE.
        let sse = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/messages"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/event-stream")
                    .set_body_raw(sse.as_bytes(), "text/event-stream"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let provider = AnthropicProvider::new(
            "sk-ant-test".to_string(),
            server.uri(),
            "claude-opus-4".to_string(),
        );
        let msgs = vec![ChatMessage {
            role: "user".to_string(),
            content: "test".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        }];
        let result = provider
            .chat_with_tools(&msgs, &[], Effort::High, &[])
            .await;
        assert!(result.is_ok(), "retry should succeed: {:?}", result.err());

        let requests = server.received_requests().await.expect("requests captured");
        assert_eq!(requests.len(), 2, "exactly 2 requests expected");
        let body1: serde_json::Value =
            serde_json::from_slice(&requests[0].body).expect("first body valid JSON");
        let body2: serde_json::Value =
            serde_json::from_slice(&requests[1].body).expect("second body valid JSON");
        assert!(
            body1.get("thinking").is_some(),
            "first request must have thinking"
        );
        assert!(
            body2.get("thinking").is_none(),
            "retry request must NOT have thinking"
        );
    }

    /// regression: verify `AnthropicSseStream` terminates
    // correctly — does NOT re-emit events infinitely after inner stream
    // ends. GeminiSseStream had this bug (infinite Done re-emit → OOM).
    // This test proves the Anthropic sibling is safe.
    // Uses `tokio::time::timeout` so a regression fails fast.
    #[tokio::test]
    async fn test_anthropic_stream_terminates() {
        let sse = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let byte_stream = make_bytes_stream(vec![sse]);
        let mut stream = AnthropicSseStream::new(byte_stream);

        use futures::StreamExt as _;
        use std::time::Duration;

        let mut events: Vec<SseEvent> = Vec::new();
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(e) => events.push(e),
                    Err(_) => break,
                }
            }
        })
        .await;

        assert!(
            result.is_ok(),
            "AnthropicSseStream must terminate. Timeout = infinite-loop regression . Events: {:?}",
            events
        );
        assert!(
            !events.is_empty(),
            "Stream should produce events. Got: {:?}",
            events
        );
    }

    #[tokio::test]
    async fn test_anthropic_body_image_block_attached() {
        // with 1 image data-URL, the last user message's content
        // must contain an `image` content block whose source.data equals
        // the base64 payload (decoded from the data-URL).
        let server = wiremock::MockServer::start().await;
        let sse = "event: content_block_delta\ndata: {\"type\":\"input_json_delta\"}\n\nevent: message_stop\ndata: {}\n\n";
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/v1/messages"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/event-stream")
                    .set_body_raw(sse.as_bytes(), "text/event-stream"),
            )
            .mount(&server)
            .await;

        let data_url = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let provider = AnthropicProvider::new(
            "test-key".to_string(),
            server.uri(),
            "claude-3-5-sonnet-latest".to_string(),
        );
        let msgs = vec![ChatMessage {
            role: "user".to_string(),
            content: "describe".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        }];
        let images = vec![data_url.to_string()];
        let _ = provider
            .chat_with_tools(&msgs, &[], Effort::None, &images)
            .await
            .expect("chat_with_tools succeeds");

        let requests = server.received_requests().await.expect("requests captured");
        let body: serde_json::Value =
            serde_json::from_slice(&requests[0].body).expect("body is valid JSON");
        let messages = body["messages"].as_array().expect("messages is array");
        let last = messages.last().expect("has last message");
        let content = last["content"].as_array().expect("content is array");

        // Find the image block.
        let image_block = content
            .iter()
            .find(|b| b["type"].as_str() == Some("image"))
            .expect("image block present");
        let source = &image_block["source"];
        assert_eq!(
            source["type"].as_str(),
            Some("base64"),
            "image source type must be base64"
        );
        assert_eq!(
            source["media_type"].as_str(),
            Some("image/png"),
            "media_type must be parsed from data-URL"
        );
        assert_eq!(
            source["data"].as_str(),
            Some(
                "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
            ),
            "base64 payload decoded from data-URL"
        );

        // And a text block with the original prompt.
        let text_block = content
            .iter()
            .find(|b| b["type"].as_str() == Some("text"))
            .expect("text block present");
        assert_eq!(
            text_block["text"].as_str(),
            Some("describe"),
            "text block must carry the original prompt"
        );
    }

    // --- Thinking signature round-trip tests (extended thinking) ---

    // Test: AnthropicSseStream captures `signature_delta` and emits
    // `SseEvent::ThinkingBlock` at `content_block_stop` with the full
    // thinking text + signature.
    #[tokio::test]
    async fn test_thinking_block_with_signature() {
        let sse = concat!(
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Let me think\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\" about this\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig_abc123\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Answer\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let byte_stream = make_bytes_stream(vec![sse]);
        let mut stream = AnthropicSseStream::new(byte_stream);
        use futures::StreamExt as _;

        let mut events = Vec::new();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(e) => events.push(e),
                    Err(_) => break,
                }
            }
        })
        .await;
        assert!(result.is_ok(), "stream did not time out");

        // Should have: Reasoning("Let me think"), Reasoning(" about this"),
        // ThinkingBlock{...}, Content("Answer"), Done
        let thinking_block = events.iter().find_map(|e| match e {
            SseEvent::ThinkingBlock {
                thinking,
                signature,
            } => Some((thinking.as_str(), signature.as_str())),
            _ => None,
        });
        assert!(
            thinking_block.is_some(),
            "ThinkingBlock event must be emitted. Got: {:?}",
            events
        );
        let (thinking, signature) = thinking_block.unwrap();
        assert_eq!(
            thinking, "Let me think about this",
            "thinking text must be accumulated from deltas"
        );
        assert_eq!(
            signature, "sig_abc123",
            "signature must be captured from signature_delta"
        );

        // Verify reasoning deltas were also emitted for live TUI display.
        let reasoning_count = events
            .iter()
            .filter(|e| matches!(e, SseEvent::Reasoning(_)))
            .count();
        assert_eq!(
            reasoning_count, 2,
            "two thinking_delta events should produce two Reasoning events"
        );

        assert!(
            events.iter().any(|e| matches!(e, SseEvent::Done)),
            "Done must be emitted"
        );
    }

    // Test: AnthropicSseStream captures `redacted_thinking` blocks and
    // emits `SseEvent::RedactedThinking` at `content_block_stop`.
    #[tokio::test]
    async fn test_redacted_thinking_block() {
        let sse = concat!(
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"encrypted_data_here\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Response\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let byte_stream = make_bytes_stream(vec![sse]);
        let mut stream = AnthropicSseStream::new(byte_stream);
        use futures::StreamExt as _;

        let mut events = Vec::new();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(e) => events.push(e),
                    Err(_) => break,
                }
            }
        })
        .await;

        let redacted = events.iter().find_map(|e| match e {
            SseEvent::RedactedThinking(data) => Some(data.as_str()),
            _ => None,
        });
        assert!(
            redacted.is_some(),
            "RedactedThinking event must be emitted. Got: {:?}",
            events
        );
        assert_eq!(
            redacted.unwrap(),
            "encrypted_data_here",
            "redacted thinking data must be captured verbatim"
        );
    }

    // Test: `convert_messages` sends back thinking blocks with signature
    // in multi-turn conversations. An assistant message with `thinking` +
    // `thinking_signature` must produce a content array with a thinking
    // block prepended.
    #[test]
    fn test_convert_messages_thinking_round_trip() {
        let messages = vec![
            ChatMessage {
                role: "user".to_string(),
                content: "What is 2+2?".to_string(),
                tool_call_id: None,
                tool_calls: None,
                attachments: None,
                thinking: None,
                thinking_signature: None,
                redacted_thinking: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: "4".to_string(),
                tool_call_id: None,
                tool_calls: None,
                attachments: None,
                thinking: Some("I need to add 2 and 2".to_string()),
                thinking_signature: Some("sig_xyz".to_string()),
                redacted_thinking: None,
            },
            ChatMessage {
                role: "user".to_string(),
                content: "Thanks!".to_string(),
                tool_call_id: None,
                tool_calls: None,
                attachments: None,
                thinking: None,
                thinking_signature: None,
                redacted_thinking: None,
            },
        ];
        let (_system, anthropic_msgs) =
            AnthropicProvider::convert_messages(&messages, &[]).unwrap();

        // The assistant message (index 1) should have a content array with
        // a thinking block followed by a text block.
        let assistant_msg = &anthropic_msgs[1];
        assert_eq!(assistant_msg["role"], "assistant");
        let content = assistant_msg["content"]
            .as_array()
            .expect("assistant content must be array when thinking present");

        // Thinking block must be first.
        assert_eq!(
            content[0]["type"].as_str(),
            Some("thinking"),
            "first content block must be thinking"
        );
        assert_eq!(
            content[0]["thinking"].as_str(),
            Some("I need to add 2 and 2"),
            "thinking text must be sent back"
        );
        assert_eq!(
            content[0]["signature"].as_str(),
            Some("sig_xyz"),
            "signature must be sent back"
        );

        // Text block must follow.
        assert_eq!(
            content[1]["type"].as_str(),
            Some("text"),
            "second content block must be text"
        );
        assert_eq!(content[1]["text"].as_str(), Some("4"));
    }

    // Test: `convert_messages` sends back redacted_thinking blocks.
    #[test]
    fn test_convert_messages_redacted_thinking_round_trip() {
        let messages = vec![ChatMessage {
            role: "assistant".to_string(),
            content: "Response".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking: None,
            thinking_signature: None,
            redacted_thinking: Some("encrypted_redacted".to_string()),
        }];
        let (_system, anthropic_msgs) =
            AnthropicProvider::convert_messages(&messages, &[]).unwrap();

        let content = anthropic_msgs[0]["content"]
            .as_array()
            .expect("content must be array when redacted_thinking present");
        assert_eq!(
            content[0]["type"].as_str(),
            Some("redacted_thinking"),
            "first block must be redacted_thinking"
        );
        assert_eq!(
            content[0]["data"].as_str(),
            Some("encrypted_redacted"),
            "redacted data must be sent back verbatim"
        );
        // Text block follows.
        assert_eq!(content[1]["type"].as_str(), Some("text"));
        assert_eq!(content[1]["text"].as_str(), Some("Response"));
    }

    // Test: assistant message with thinking + tool_calls produces content
    // array with thinking block first, then text, then tool_use.
    #[test]
    fn test_convert_messages_thinking_with_tool_calls() {
        let messages = vec![ChatMessage {
            role: "assistant".to_string(),
            content: "Let me check".to_string(),
            tool_call_id: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                call_type: "function".to_string(),
                function: ToolCallFunction {
                    name: "bash".to_string(),
                    arguments: r#"{"command":"ls"}"#.to_string(),
                },
            }]),
            attachments: None,
            thinking: Some("I should run ls".to_string()),
            thinking_signature: Some("sig_123".to_string()),
            redacted_thinking: None,
        }];
        let (_system, anthropic_msgs) =
            AnthropicProvider::convert_messages(&messages, &[]).unwrap();

        let content = anthropic_msgs[0]["content"]
            .as_array()
            .expect("content must be array");
        // Order: thinking → text → tool_use
        assert_eq!(content[0]["type"].as_str(), Some("thinking"));
        assert_eq!(content[1]["type"].as_str(), Some("text"));
        assert_eq!(content[2]["type"].as_str(), Some("tool_use"));
    }
}
