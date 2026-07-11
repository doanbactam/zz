//! LLM provider abstraction for ZeroZero.
//!
//! `Provider` trait with `chat_stream` (single-turn, no tools).
//! `chat_with_tools` method for multi-turn with tool support.
//! `SseDeltaStream` extended to accumulate tool call arguments and yield
//! `SseEvent` (Content, ToolCall, Done).

pub mod anthropic;
pub mod auth;
pub mod fallback;
pub mod gemini;
pub mod providers;
pub mod sse;

use base64::Engine;
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::pin::Pin;

pub use anthropic::AnthropicProvider;
pub use auth::{
    AuthEntry, AuthStore, KeySource, auth_path, has_api_key, key_source, resolve_api_key,
    resolve_api_key_for_spec, resolve_base_url, resolve_model,
};
pub use fallback::FallbackProvider;
pub use gemini::GeminiProvider;
pub use providers::{
    DEFAULT_PROVIDER_ID, PROVIDERS, ProviderKind, ProviderSpec, find_provider, provider_ids,
    provider_spec, resolve_provider_id,
};
pub use sse::SseEvent;

// Reasoning effort level for LLM providers).
// Controls how much "thinking" a model does before responding. Mapped to
// provider-native parameters:
// - OpenAI (Chat Completions): `reasoning_effort` (top-level string).
// - Anthropic: `thinking: { type: "enabled", budget_tokens: N }` + raised
//   `max_tokens`.
// - Gemini: no-op (MVP — `thinkingConfig` deferred).
// `None` (default) preserves prior behavior: no reasoning parameter is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Effort {
    // No reasoning parameter sent (provider default). Preserves behavior
    // for non-reasoning models (gpt-4o, grok-4, ollama local, etc.).
    #[default]
    None,
    // Low reasoning — fast/cheap, minimal thinking.
    Low,
    // Medium reasoning — balanced quality/latency (OpenAI/Codex default).
    Medium,
    // High reasoning — deep thinking for hard tasks.
    High,
}

impl Effort {
    // Map effort to an Anthropic `budget_tokens` value.
    // Returns `None` for `Effort::None` (no thinking field).
    pub const fn budget_tokens(&self) -> Option<u32> {
        match self {
            Self::None => None,
            Self::Low => Some(2000),
            Self::Medium => Some(8000),
            Self::High => Some(16000),
        }
    }

    // Map effort to a Gemini `thinkingBudget` token value.
    // Returns `None` for `Effort::None` (no thinkingConfig sent).
    // Values are heuristic, within supported ranges for Gemini 2.5
    // Flash (1–24576) and Pro (128–32768):
    // - Low → 1024 (minimal thinking, fast)
    // - Medium → 8192 (default auto, moderate)
    // - High → 24576 (deep thinking, within both Flash and Pro max)
    pub const fn thinking_budget(&self) -> Option<u32> {
        match self {
            Self::None => None,
            Self::Low => Some(1024),
            Self::Medium => Some(8192),
            Self::High => Some(24576),
        }
    }

    // Heuristic denylist of models that do NOT support reasoning effort.
    // Returns `false` for known non-reasoning models, `true` otherwise.
    // Denylist (exact + prefix match): `grok-3` (exact), `grok-4`
    // (exact), `grok-3-`, `grok-4-`, `gpt-4o-mini`, `gpt-4o-`,
    // `gpt-3.5`. Newer reasoning-capable models (gpt-5, o3, grok-4.3,
    // claude-opus-4, claude-sonnet-4) default to `true`.
    pub fn supports_reasoning(model: &str) -> bool {
        const DENYLIST_PREFIX: &[&str] =
            &["grok-3-", "grok-4-", "gpt-4o-mini", "gpt-4o-", "gpt-3.5"];
        const DENYLIST_EXACT: &[&str] = &["grok-3", "grok-4"];
        !DENYLIST_PREFIX.iter().any(|p| model.starts_with(p)) && !DENYLIST_EXACT.contains(&model)
    }
}

impl std::fmt::Display for Effort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
        }
    }
}

impl std::str::FromStr for Effort {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "none" => Ok(Self::None),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            other => Err(format!(
                "invalid effort '{other}' (expected none|low|medium|high)"
            )),
        }
    }
}

// A stream of content deltas (token-by-token) from an LLM provider.
// Used by `chat_stream` (backward compat).
pub type DeltaStream = Pin<Box<dyn Stream<Item = anyhow::Result<String>> + Send>>;

// A stream of `SseEvent` items (content, tool calls, done) from an LLM
// provider. Used by `chat_with_tools` .
pub type SseEventStream = Pin<Box<dyn Stream<Item = anyhow::Result<SseEvent>> + Send>>;

// A chat message in the conversation history. Used for multi-turn
// conversations with tool support.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    // Images attached to this message (TUI multimodal composer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<ImageAttachment>>,
    // Anthropic extended thinking text content. When present along with
    // `thinking_signature`, sent back as a thinking content block for
    // multi-turn extended thinking. Captured from `thinking_delta` SSE
    // events and stored for round-trip in `convert_messages`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    // Anthropic thinking block signature — cryptographic verification
    // token for thinking blocks. Required for multi-turn conversations
    // with extended thinking. Sent back to Anthropic API verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
    // Anthropic redacted thinking — encrypted thinking content that
    // was flagged by safety systems. Must be sent back verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted_thinking: Option<String>,
}

// A tool call in an assistant message.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ToolCallFunction,
}

// The function part of a tool call.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

// An image attached to a chat message (TUI multimodal composer).
// Stored as a full `data:<mime>;base64,<...>` URL, which is exactly the
// form expected by every provider's vision API.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ImageAttachment {
    // Optional original filename (for display / sanity).
    pub filename: Option<String>,
    // Full data-URL: `data:<mime>;base64,<...>`.
    pub data_url: String,
}

impl ImageAttachment {
    // Build from an existing data-URL string.
    pub fn from_data_url(url: impl Into<String>) -> Self {
        Self {
            filename: None,
            data_url: url.into(),
        }
    }

    // Build from raw bytes + a mime type (encodes as base64 data-URL).
    pub fn from_bytes_with_name(
        name: Option<String>,
        mime: impl Into<String>,
        bytes: &[u8],
    ) -> Self {
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        Self {
            filename: name,
            data_url: format!("data:{};base64,{}", mime.into(), encoded),
        }
    }
}

// Returns true if `s` looks like an `data:image/...;base64,...` data-URL.
pub fn looks_like_image_data_url(s: &str) -> bool {
    s.starts_with("data:image/") && s.contains(";base64,")
}

// Extract the raw data-URL strings from a slice of attachments.
pub fn data_urls_of(attachments: &[ImageAttachment]) -> Vec<String> {
    attachments.iter().map(|a| a.data_url.clone()).collect()
}

// Best-effort mime guess from a file path extension.
pub fn mime_from_path(path: &std::path::Path) -> String {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    }
    .to_string()
}

// Collect image data-URLs from a full message history. Used by `run_turn`.
pub fn collect_turn_image_urls(messages: &[ChatMessage]) -> Vec<String> {
    let mut out = Vec::new();
    for m in messages {
        if let Some(atts) = m.attachments.as_deref() {
            out.extend(data_urls_of(atts));
        }
    }
    out
}

// Abstract LLM provider.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    // Stream chat completion tokens for a single user prompt .
    // Yields incremental content deltas. No tools, no multi-turn.
    async fn chat_stream(&self, prompt: &str) -> anyhow::Result<DeltaStream>;

    // Stream chat completion with tools support .
    // `messages` is the full conversation history (user, assistant, tool).
    // `tools` is the OpenAI tools array (JSON function definitions).
    // `effort` controls reasoning effort . `Effort::None` preserves
    // prior behavior (no reasoning parameter sent).
    // `images` is a slice of image data-URLs (e.g.
    // `data:image/png;base64,<...>`) to attach to the conversation for
    // multimodal / vision input . Each provider maps the list to
    // its native vision format. Empty slice = no images sent.
    // Returns a stream of `SseEvent` items.
    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        effort: Effort,
        images: &[String],
    ) -> anyhow::Result<SseEventStream>;

    // Return the current model name . Used for `/model` command
    // testing and display. Default implementation returns an empty string
    // for backward compatibility with providers that do not override it.
    fn model(&self) -> &str {
        ""
    }
}

// OpenAI Chat Completions provider with SSE streaming.
#[derive(Clone)]
pub struct OpenAIProvider {
    api_key: String,
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl OpenAIProvider {
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
}

#[async_trait::async_trait]
impl Provider for OpenAIProvider {
    async fn chat_stream(&self, prompt: &str) -> anyhow::Result<DeltaStream> {
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
        // Convert SseEvent stream to String stream (content only).
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
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        // attach images to the conversation. OpenAI vision format:
        // the last user message's `content` becomes an array of parts; each
        // image is an {type:"image_url", image_url:{url: <data-url>}} part,
        // and the text (if any) is a {type:"text", text: ...} part. Messages
        // are serialized via serde_json, so we build a serde_json::Value
        // array here (replacing the auto-serialized `messages`).
        let messages_json: serde_json::Value = if images.is_empty() {
            serde_json::to_value(messages)?
        } else {
            let mut converted: Vec<serde_json::Value> = Vec::with_capacity(messages.len());
            for (idx, msg) in messages.iter().enumerate() {
                let is_last_user = idx == messages.len() - 1 && msg.role == "user";
                if is_last_user && !images.is_empty() {
                    // Build multimodal content array.
                    let mut parts: Vec<serde_json::Value> = Vec::new();
                    if !msg.content.is_empty() {
                        parts.push(serde_json::json!({
                            "type": "text",
                            "text": msg.content,
                        }));
                    }
                    for img in images {
                        parts.push(serde_json::json!({
                            "type": "image_url",
                            "image_url": { "url": img },
                        }));
                    }
                    let mut entry = serde_json::json!({
                        "role": msg.role,
                        "content": parts,
                    });
                    if let Some(tc) = &msg.tool_calls {
                        entry["tool_calls"] = serde_json::to_value(tc)?;
                    }
                    converted.push(entry);
                } else {
                    converted.push(serde_json::to_value(msg)?);
                }
            }
            serde_json::Value::Array(converted)
        };

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": messages_json,
            "stream": true,
        });
        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(tools.to_vec());
        }
        //A: Chat Completions uses top-level `reasoning_effort` string.
        // Only send if effort != None AND the model supports reasoning
        // (heuristic denylist to avoid 400s on gpt-4o/grok-4/etc.).
        if effort != Effort::None && Effort::supports_reasoning(&self.model) {
            body["reasoning_effort"] = serde_json::Value::String(effort.to_string());
        }

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        //E: If the model rejects reasoning_effort with HTTP 400 and
        // effort was set, retry once without it (Effort::None) to recover.
        if response.status().as_u16() == 400 && effort != Effort::None {
            return self
                .chat_with_tools(messages, tools, Effort::None, images)
                .await;
        }

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("LLM HTTP error {status}: {text}");
        }

        let byte_stream = response.bytes_stream();
        let stream = SseEventDeltaStream::new(byte_stream);
        Ok(Box::pin(stream))
    }

    fn model(&self) -> &str {
        &self.model
    }
}

// Adapter that converts a `reqwest::Bytes` stream into a stream of
// `SseEvent` items. Handles content deltas, tool call accumulation,
// and the `[DONE]` marker.
struct SseEventDeltaStream<S> {
    inner: S,
    buffer: String,
    // Accumulated tool calls: index -> (id, name, arguments_accumulated).
    tool_calls: HashMap<usize, (String, String, String)>,
    // Whether we've seen finish_reason: "tool_calls".
    tool_calls_finished: bool,
    // Pending events queued from process_line but not yet emitted (pending queue fix).
    // A single SSE line can produce multiple events (e.g. flush_tool_calls
    // yields N ToolCall events + 1 Done). Without this queue, poll_next
    // would drop all but the first.
    pending: VecDeque<anyhow::Result<SseEvent>>,
}

impl<S> SseEventDeltaStream<S>
where
    S: Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin + Send,
{
    fn new(inner: S) -> Self {
        Self {
            inner,
            buffer: String::new(),
            tool_calls: HashMap::new(),
            tool_calls_finished: false,
            pending: VecDeque::new(),
        }
    }

    // Process a complete line and return any events to emit.
    fn process_line(&mut self, line: &str) -> Vec<anyhow::Result<SseEvent>> {
        let mut events = Vec::new();

        // Check for content or done.
        if let Some(event) = sse::parse_sse_line(line) {
            match &event {
                SseEvent::Done => {
                    // Flush any accumulated tool calls.
                    self.flush_tool_calls(&mut events);
                    events.push(Ok(event));
                    return events;
                }
                SseEvent::Content(_) | SseEvent::Reasoning(_) => {
                    events.push(Ok(event));
                    return events;
                }
                SseEvent::ToolCall { .. } => {
                    // Shouldn't happen from parse_sse_line, but pass through.
                    events.push(Ok(event));
                    return events;
                }
                // ThinkingBlock / RedactedThinking are only produced by
                // AnthropicSseStream, not by parse_sse_line (OpenAI). Pass
                // through if they ever appear here.
                SseEvent::ThinkingBlock { .. } | SseEvent::RedactedThinking(_) => {
                    events.push(Ok(event));
                    return events;
                }
            }
        }

        // Check for tool_calls delta.
        if let Some((index, id, name, args_fragment)) = sse::extract_tool_call_delta(line) {
            let entry = self
                .tool_calls
                .entry(index)
                .or_insert_with(|| (String::new(), String::new(), String::new()));
            if let Some(i) = id {
                entry.0 = i;
            }
            if let Some(n) = name {
                entry.1 = n;
            }
            entry.2.push_str(&args_fragment);
            return events;
        }

        // Check for finish_reason: "tool_calls".
        if sse::has_tool_calls_finish(line) {
            self.tool_calls_finished = true;
            self.flush_tool_calls(&mut events);
            return events;
        }

        events
    }

    // Flush accumulated tool calls as SseEvent::ToolCall events.
    fn flush_tool_calls(&mut self, events: &mut Vec<anyhow::Result<SseEvent>>) {
        let mut indices: Vec<usize> = self.tool_calls.keys().copied().collect();
        indices.sort();
        for index in indices {
            if let Some((id, name, args)) = self.tool_calls.remove(&index) {
                events.push(Ok(SseEvent::ToolCall {
                    id,
                    name,
                    arguments: args,
                }));
            }
        }
    }
}

impl<S> Stream for SseEventDeltaStream<S>
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
            // 1. Pop from pending queue first (pending queue fix).
            // A single process_line call may have queued multiple events.
            if let Some(event) = this.pending.pop_front() {
                return std::task::Poll::Ready(Some(event));
            }

            // 2. Try to extract a complete line from the buffer.
            if let Some(nl) = this.buffer.find('\n') {
                let line = this.buffer[..nl].to_string();
                this.buffer = this.buffer[nl + 1..].to_string();
                let events = this.process_line(&line);
                // Push ALL events into pending queue (not just first).
                // Loop will pop from pending on next iteration.
                this.pending.extend(events);
                continue;
            }

            // 3. Need more data from the inner stream.
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
                        let line = this.buffer.clone();
                        this.buffer.clear();
                        let events = this.process_line(&line);
                        this.pending.extend(events);
                    }
                    // Flush any remaining tool calls.
                    if !this.tool_calls.is_empty() {
                        let mut events = Vec::new();
                        this.flush_tool_calls(&mut events);
                        this.pending.extend(events);
                    }
                    // If pending now has events, loop will pop them.
                    // Otherwise signal end.
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

// Need futures::StreamExt for filter_map in chat_stream.
use futures::StreamExt;

#[cfg(test)]
mod tests_prd8 {
    //! Tests for : mutation coverage on SSE flush + edge cases.
    //!
    //! These tests target mutants:
    //! - flush_tool_calls replaced with () → AC-5 catches
    //! - process_line returning empty vec → AC-5/6 catch
    //! - poll_next arithmetic mutations → AC-5/6 catch

    use super::*;
    use futures::stream;

    // Helper: build a fake bytes-stream from a sequence of string chunks.
    fn make_bytes_stream(
        chunks: Vec<&str>,
    ) -> impl Stream<Item = Result<bytes::Bytes, reqwest::Error>> {
        stream::iter(
            chunks
                .into_iter()
                .map(|s| Ok(bytes::Bytes::from(s.to_string()))),
        )
    }

    #[tokio::test]
    async fn test_prd8_ac5_sse_flush_tool_calls_on_done() {
        // Tool-call delta arrives, then [DONE]. Expect ToolCall event then Done.
        // FIXED : both events now emit via pending queue.
        let lines = [
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_42\",\"function\":{\"name\":\"bash\",\"arguments\":\"{\\\"cmd\\\":\\\"ls\\\"}\"}}]}}]}\n",
            "data: [DONE]\n",
        ];
        let joined = lines.join("");
        let byte_stream = make_bytes_stream(vec![joined.as_str()]);
        let mut sse_stream = SseEventDeltaStream::new(byte_stream);

        use futures::StreamExt as _;
        let mut events: Vec<SseEvent> = Vec::new();
        while let Some(item) = sse_stream.next().await {
            match item {
                Ok(e) => events.push(e),
                Err(_) => break,
            }
        }

        // ToolCall must be emitted on flush.
        let has_tool_call = events.iter().any(|e| {
            matches!(
                e,
                SseEvent::ToolCall { name, .. } if name == "bash"
            )
        });
        assert!(
            has_tool_call,
            "ToolCall(bash) must be flushed before Done. Got: {:?}",
            events
        );

        // Done must also be emitted (pending queue fix — previously dropped).
        assert!(
            events.iter().any(|e| matches!(e, SseEvent::Done)),
            "Done must be emitted after ToolCall. Got: {:?}",
            events
        );

        // Order: ToolCall before Done.
        let tc_idx = events
            .iter()
            .position(|e| matches!(e, SseEvent::ToolCall { .. }))
            .unwrap();
        let done_idx = events
            .iter()
            .position(|e| matches!(e, SseEvent::Done))
            .unwrap();
        assert!(
            tc_idx < done_idx,
            "ToolCall must come before Done. Got positions: tc={}, done={}",
            tc_idx,
            done_idx
        );
    }

    #[tokio::test]
    async fn test_prd8_ac5_sse_flush_tool_calls_on_finish_reason() {
        // Tool-call delta arrives, then finish_reason: "tool_calls", then [DONE].
        let lines = [
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_99\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{}\"}}]}}]}\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
            "data: [DONE]\n",
        ];
        let joined = lines.join("");
        let byte_stream = make_bytes_stream(vec![joined.as_str()]);
        let mut sse_stream = SseEventDeltaStream::new(byte_stream);

        use futures::StreamExt as _;
        let mut events: Vec<SseEvent> = Vec::new();
        while let Some(item) = sse_stream.next().await {
            match item {
                Ok(e) => events.push(e),
                Err(_) => break,
            }
        }

        let has_tool_call = events.iter().any(|e| {
            matches!(
                e,
                SseEvent::ToolCall { name, .. } if name == "read_file"
            )
        });
        assert!(
            has_tool_call,
            "ToolCall(read_file) must be flushed on finish_reason. Got: {:?}",
            events
        );
    }

    #[tokio::test]
    async fn test_prd8_ac6_sse_empty_delta_emits_nothing() {
        // Role-only delta (no content, no tool_calls) → parse_sse_line returns None.
        // Stream should not emit any Content event for it.
        let lines = [
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n",
            "data: [DONE]\n",
        ];
        let joined = lines.join("");
        let byte_stream = make_bytes_stream(vec![joined.as_str()]);
        let mut sse_stream = SseEventDeltaStream::new(byte_stream);

        use futures::StreamExt as _;
        let mut events: Vec<SseEvent> = Vec::new();
        while let Some(item) = sse_stream.next().await {
            match item {
                Ok(e) => events.push(e),
                Err(_) => break,
            }
        }

        // Only one Content event ("hi") and Done. No phantom event from role delta.
        let content_count = events
            .iter()
            .filter(|e| matches!(e, SseEvent::Content(_)))
            .count();
        assert_eq!(
            content_count, 1,
            "Exactly one Content event expected. Got: {:?}",
            events
        );
        if let Some(SseEvent::Content(text)) = events.first() {
            assert_eq!(text, "hi");
        }
    }

    #[tokio::test]
    async fn test_prd8_ac6_sse_partial_chunk_accumulated() {
        // Content delivered across two SSE lines: "hel" + "lo".
        let lines = [
            "data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n",
            "data: [DONE]\n",
        ];
        let joined = lines.join("");
        let byte_stream = make_bytes_stream(vec![joined.as_str()]);
        let mut sse_stream = SseEventDeltaStream::new(byte_stream);

        use futures::StreamExt as _;
        let mut events: Vec<SseEvent> = Vec::new();
        while let Some(item) = sse_stream.next().await {
            match item {
                Ok(e) => events.push(e),
                Err(_) => break,
            }
        }

        // Expect two Content events (one per delta). Mutant changing buffer
        // accumulation would cause one to be dropped.
        let contents: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                SseEvent::Content(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            contents,
            vec!["hel".to_string(), "lo".to_string()],
            "Both content deltas must be emitted separately. Got: {:?}",
            events
        );
    }

    #[tokio::test]
    async fn test_prd8_ac6_sse_tool_call_split_arguments() {
        // Tool call arguments split across multiple deltas (typical OpenAI).
        let lines = [
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"bash\",\"arguments\":\"\"}}]}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"x\\\":\"}}]}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"1}\"}}]}}]}\n",
            "data: [DONE]\n",
        ];
        let joined = lines.join("");
        let byte_stream = make_bytes_stream(vec![joined.as_str()]);
        let mut sse_stream = SseEventDeltaStream::new(byte_stream);

        use futures::StreamExt as _;
        let mut events: Vec<SseEvent> = Vec::new();
        while let Some(item) = sse_stream.next().await {
            match item {
                Ok(e) => events.push(e),
                Err(_) => break,
            }
        }

        // Find the ToolCall event and verify arguments are concatenated.
        let tool_call = events.iter().find_map(|e| match e {
            SseEvent::ToolCall {
                id,
                name,
                arguments,
            } => Some((id.clone(), name.clone(), arguments.clone())),
            _ => None,
        });
        let (id, name, args) = tool_call.expect("ToolCall event must be emitted");
        assert_eq!(id, "c1");
        assert_eq!(name, "bash");
        assert_eq!(args, r#"{"x":1}"#);
    }

    #[tokio::test]
    async fn test_prd8_ac5_sse_no_tool_calls_emits_only_done() {
        // No tool calls at all → Done only. Mutant "flush empty" should not
        // emit phantom tool calls.
        let lines = [
            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n",
            "data: [DONE]\n",
        ];
        let joined = lines.join("");
        let byte_stream = make_bytes_stream(vec![joined.as_str()]);
        let mut sse_stream = SseEventDeltaStream::new(byte_stream);

        use futures::StreamExt as _;
        let mut events: Vec<SseEvent> = Vec::new();
        while let Some(item) = sse_stream.next().await {
            match item {
                Ok(e) => events.push(e),
                Err(_) => break,
            }
        }

        let tool_call_count = events
            .iter()
            .filter(|e| matches!(e, SseEvent::ToolCall { .. }))
            .count();
        assert_eq!(
            tool_call_count, 0,
            "No ToolCall events expected. Got: {:?}",
            events
        );
    }

    // --- : fix — pending event queue tests ---

    #[tokio::test]
    async fn test_prd10_ac1_multi_tool_call_flush_then_done() {
        // 2 tool_calls (index 0, 1) + [DONE]. Both ToolCall events + Done
        // must emit. Before fix, only first ToolCall survived.
        let lines = [
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c0\",\"function\":{\"name\":\"bash\",\"arguments\":\"{}\"}}]}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"c1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{}\"}}]}}]}\n",
            "data: [DONE]\n",
        ];
        let joined = lines.join("");
        let byte_stream = make_bytes_stream(vec![joined.as_str()]);
        let mut sse_stream = SseEventDeltaStream::new(byte_stream);

        use futures::StreamExt as _;
        let mut events: Vec<SseEvent> = Vec::new();
        while let Some(item) = sse_stream.next().await {
            match item {
                Ok(e) => events.push(e),
                Err(_) => break,
            }
        }

        // Expect: 2 ToolCall + 1 Done = 3 events total.
        let tool_calls: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                SseEvent::ToolCall { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            tool_calls,
            vec!["bash", "read_file"],
            "Both tool calls must emit in index order. Got: {:?}",
            events
        );
        assert!(
            events.iter().any(|e| matches!(e, SseEvent::Done)),
            "Done must emit. Got: {:?}",
            events
        );
        assert_eq!(
            events.len(),
            3,
            "Exactly 3 events: 2 ToolCall + 1 Done. Got: {:?}",
            events
        );
    }

    #[tokio::test]
    async fn test_prd10_ac2_stream_end_flush_remaining() {
        // Tool-call delta, then stream ends (no [DONE]). ToolCall must
        // be flushed on stream end.
        let lines = [
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c_end\",\"function\":{\"name\":\"write_file\",\"arguments\":\"{}\"}}]}}]}\n",
        ];
        let joined = lines.join("");
        let byte_stream = make_bytes_stream(vec![joined.as_str()]);
        let mut sse_stream = SseEventDeltaStream::new(byte_stream);

        use futures::StreamExt as _;
        let mut events: Vec<SseEvent> = Vec::new();
        while let Some(item) = sse_stream.next().await {
            match item {
                Ok(e) => events.push(e),
                Err(_) => break,
            }
        }

        let has_tool_call = events.iter().any(|e| {
            matches!(
                e,
                SseEvent::ToolCall { name, .. } if name == "write_file"
            )
        });
        assert!(
            has_tool_call,
            "ToolCall must flush on stream end. Got: {:?}",
            events
        );
    }

    #[tokio::test]
    async fn test_prd10_ac3_finish_reason_multi_tool_call_then_done() {
        // 2 tool_calls + finish_reason:tool_calls + [DONE].
        // finish_reason flushes 2 ToolCalls, then Done emits.
        let lines = [
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"f0\",\"function\":{\"name\":\"bash\",\"arguments\":\"{}\"}}]}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"f1\",\"function\":{\"name\":\"grep\",\"arguments\":\"{}\"}}]}}]}\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
            "data: [DONE]\n",
        ];
        let joined = lines.join("");
        let byte_stream = make_bytes_stream(vec![joined.as_str()]);
        let mut sse_stream = SseEventDeltaStream::new(byte_stream);

        use futures::StreamExt as _;
        let mut events: Vec<SseEvent> = Vec::new();
        while let Some(item) = sse_stream.next().await {
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
            "Both tool calls must flush on finish_reason. Got: {:?}",
            events
        );
        assert!(
            events.iter().any(|e| matches!(e, SseEvent::Done)),
            "Done must emit. Got: {:?}",
            events
        );
        assert_eq!(
            events.len(),
            3,
            "3 events: 2 ToolCall + 1 Done. Got: {:?}",
            events
        );
    }

    #[tokio::test]
    async fn test_prd10_ac4_single_event_no_duplication() {
        // Single content delta + Done. Exactly 1 Content + 1 Done = 2.
        // No duplication from pending queue.
        let lines = [
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n",
            "data: [DONE]\n",
        ];
        let joined = lines.join("");
        let byte_stream = make_bytes_stream(vec![joined.as_str()]);
        let mut sse_stream = SseEventDeltaStream::new(byte_stream);

        use futures::StreamExt as _;
        let mut events: Vec<SseEvent> = Vec::new();
        while let Some(item) = sse_stream.next().await {
            match item {
                Ok(e) => events.push(e),
                Err(_) => break,
            }
        }

        assert_eq!(
            events.len(),
            2,
            "Exactly 2 events: 1 Content + 1 Done. Got: {:?}",
            events
        );
        assert!(matches!(events[0], SseEvent::Content(ref s) if s == "hello"));
        assert!(matches!(events[1], SseEvent::Done));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_provider_construction() {
        let p = OpenAIProvider::new(
            "test-key".to_string(),
            "http://localhost:8080/v1".to_string(),
            "test-model".to_string(),
        );
        assert_eq!(p.api_key, "test-key");
        assert_eq!(p.base_url, "http://localhost:8080/v1");
        assert_eq!(p.model, "test-model");
    }

    #[test]
    fn test_chat_message_serialization() {
        let msg = ChatMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"role\":\"user\""));
        assert!(json.contains("\"content\":\"hello\""));
        assert!(!json.contains("tool_call_id"));
        assert!(!json.contains("tool_calls"));
    }

    #[test]
    fn test_chat_message_with_tool_result() {
        let msg = ChatMessage {
            role: "tool".to_string(),
            content: "file contents here".to_string(),
            tool_call_id: Some("call_abc".to_string()),
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"role\":\"tool\""));
        assert!(json.contains("\"tool_call_id\":\"call_abc\""));
    }

    #[test]
    fn test_gemini_provider_construction() {
        let p = GeminiProvider::new("AIzaSyTest".to_string(), "gemini-2.0-flash".to_string());
        assert_eq!(p.api_key, "AIzaSyTest");
        assert_eq!(p.model, "gemini-2.0-flash");
    }

    // --- : reasoning effort control — full test bodies ---
    // Written by B (Test Author, Round 7) based on + design only.

    // AC-1: Effort::Display produces correct lowercase strings.
    #[test]
    fn test_effort_display() {
        assert_eq!(Effort::None.to_string(), "none");
        assert_eq!(Effort::Low.to_string(), "low");
        assert_eq!(Effort::Medium.to_string(), "medium");
        assert_eq!(Effort::High.to_string(), "high");
    }

    // AC-1: Effort::FromStr parses case-insensitive, rejects invalid.
    #[test]
    fn test_effort_fromstr() {
        assert_eq!("low".parse::<Effort>().unwrap(), Effort::Low);
        assert_eq!("medium".parse::<Effort>().unwrap(), Effort::Medium);
        assert_eq!("high".parse::<Effort>().unwrap(), Effort::High);
        assert_eq!("none".parse::<Effort>().unwrap(), Effort::None);
        // Case-insensitive.
        assert_eq!("HIGH".parse::<Effort>().unwrap(), Effort::High);
        assert_eq!("None".parse::<Effort>().unwrap(), Effort::None);
        assert_eq!("  LoW  ".parse::<Effort>().unwrap(), Effort::Low);
        // Invalid → Err.
        assert!("xhigh".parse::<Effort>().is_err());
        assert!("".parse::<Effort>().is_err());
        assert!("medium2".parse::<Effort>().is_err());
    }

    // AC-1/AC-3: Effort::budget_tokens mapping (None→None, Low→2000,
    // Medium→8000, High→16000).
    #[test]
    fn test_effort_budget_tokens() {
        assert_eq!(Effort::None.budget_tokens(), None);
        assert_eq!(Effort::Low.budget_tokens(), Some(2000));
        assert_eq!(Effort::Medium.budget_tokens(), Some(8000));
        assert_eq!(Effort::High.budget_tokens(), Some(16000));
    }

    // Test: Effort::thinking_budget() returns Gemini thinkingBudget values.
    // None → None (no thinkingConfig), Low → 1024, Medium → 8192, High → 24576.
    #[test]
    fn test_effort_thinking_budget() {
        assert_eq!(Effort::None.thinking_budget(), None);
        assert_eq!(Effort::Low.thinking_budget(), Some(1024));
        assert_eq!(Effort::Medium.thinking_budget(), Some(8192));
        assert_eq!(Effort::High.thinking_budget(), Some(24576));
    }

    // AC-5: supports_reasoning denylist — grok-3 (exact) denied,
    // grok-4 (exact) denied, grok-3-* denied, grok-4-* denied,
    // gpt-4o-mini denied, gpt-4o-* denied, gpt-3.5* denied. gpt-5,
    // o3, claude-opus-4, grok-4.3 supported.
    #[test]
    fn test_supports_reasoning_denylist() {
        // Denied (non-reasoning models).
        assert!(!Effort::supports_reasoning("grok-3"), "exact grok-3 denied");
        assert!(!Effort::supports_reasoning("grok-4"), "exact grok-4 denied");
        assert!(!Effort::supports_reasoning("grok-3-turbo"));
        assert!(!Effort::supports_reasoning("grok-4-0709-beta"));
        assert!(!Effort::supports_reasoning("grok-4-xxx"));
        assert!(!Effort::supports_reasoning("gpt-4o-mini"));
        assert!(!Effort::supports_reasoning("gpt-4o-2024-11-20"));
        assert!(!Effort::supports_reasoning("gpt-3.5-turbo"));
        // Supported (reasoning-capable models).
        assert!(Effort::supports_reasoning("gpt-5"));
        assert!(Effort::supports_reasoning("o3"));
        assert!(Effort::supports_reasoning("claude-opus-4"));
        assert!(Effort::supports_reasoning("grok-4.3"));
    }

    // AC-1: Effort::default() == Effort::None (CLI default preserves
    // behavior for non-reasoning models). TUI sets Medium explicitly.
    #[test]
    fn test_effort_default_none() {
        assert_eq!(
            Effort::default(),
            Effort::None,
            "Effort::default() must be None (CLI default, preserve behavior)"
        );
    }

    // AC-2: OpenAI body contains top-level "reasoning_effort" string when
    // effort != None and model supports reasoning. Uses wiremock to
    // capture the actual request body.A: field is top-level string.
    #[tokio::test]
    async fn test_openai_body_reasoning_effort() {
        let server = wiremock::MockServer::start().await;
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/event-stream")
                    .set_body_raw(sse.as_bytes(), "text/event-stream"),
            )
            .mount(&server)
            .await;

        let provider = OpenAIProvider::new(
            "test-key".to_string(),
            server.uri(),
            "gpt-5".to_string(), // supports reasoning
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
        assert!(!requests.is_empty(), "at least one request sent");
        let body: serde_json::Value =
            serde_json::from_slice(&requests[0].body).expect("body is valid JSON");
        //A: top-level string, not nested object.
        assert_eq!(
            body["reasoning_effort"].as_str(),
            Some("high"),
            "reasoning_effort must be top-level string 'high'"
        );
        // Verify it's a string, not an object (GAP-A).
        assert!(
            body["reasoning_effort"].is_string(),
            "reasoning_effort must be a string, not an object"
        );
    }

    // AC-2: When effort == None, body must NOT contain reasoning_effort.
    #[tokio::test]
    async fn test_openai_body_no_effort_when_none() {
        let server = wiremock::MockServer::start().await;
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/event-stream")
                    .set_body_raw(sse.as_bytes(), "text/event-stream"),
            )
            .mount(&server)
            .await;

        let provider =
            OpenAIProvider::new("test-key".to_string(), server.uri(), "gpt-5".to_string());
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
            body.get("reasoning_effort").is_none(),
            "reasoning_effort must NOT be present when effort is None. Got: {body}"
        );
    }

    // AC-5: When model is in denylist (grok-4-xxx), effort=High must NOT
    // produce reasoning_effort in body (denylist skip).
    #[tokio::test]
    async fn test_openai_body_no_effort_when_denylist() {
        let server = wiremock::MockServer::start().await;
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/event-stream")
                    .set_body_raw(sse.as_bytes(), "text/event-stream"),
            )
            .mount(&server)
            .await;

        let provider = OpenAIProvider::new(
            "test-key".to_string(),
            server.uri(),
            "grok-4-xxx".to_string(), // in denylist
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
        let body: serde_json::Value =
            serde_json::from_slice(&requests[0].body).expect("body is valid JSON");
        assert!(
            body.get("reasoning_effort").is_none(),
            "reasoning_effort must NOT be sent for denylisted model grok-4-xxx. Got: {body}"
        );
    }

    // AC-5: When model rejects reasoning_effort with HTTP 400, ZeroZero
    // retries without effort. Verify two requests sent: first with
    // reasoning_effort, second without.
    #[tokio::test]
    async fn test_openai_400_retry_without_effort() {
        let server = wiremock::MockServer::start().await;

        // First request (with reasoning_effort) → 400.
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .and(wiremock::matchers::body_string_contains("reasoning_effort"))
            .respond_with(wiremock::ResponseTemplate::new(400).set_body_string(
                r#"{"error":{"message":"reasoning_effort not supported for this model"}}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;

        // Second request (without reasoning_effort) → 200 SSE.
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/event-stream")
                    .set_body_raw(sse.as_bytes(), "text/event-stream"),
            )
            .expect(1)
            .mount(&server)
            .await;

        // Use a model NOT in denylist so reasoning_effort is sent first.
        let provider =
            OpenAIProvider::new("test-key".to_string(), server.uri(), "gpt-5".to_string());
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

        // Verify two requests were sent.
        let requests = server.received_requests().await.expect("requests captured");
        assert_eq!(
            requests.len(),
            2,
            "exactly 2 requests expected (first 400, retry 200)"
        );
        let body1: serde_json::Value =
            serde_json::from_slice(&requests[0].body).expect("first body valid JSON");
        let body2: serde_json::Value =
            serde_json::from_slice(&requests[1].body).expect("second body valid JSON");
        assert!(
            body1.get("reasoning_effort").is_some(),
            "first request must have reasoning_effort"
        );
        assert!(
            body2.get("reasoning_effort").is_none(),
            "retry request must NOT have reasoning_effort"
        );
    }

    /// regression: verify `SseEventDeltaStream` (OpenAI)
    // terminates correctly — does NOT re-emit events infinitely after
    // inner stream ends. GeminiSseStream had this bug (infinite Done
    // re-emit → OOM). This test proves the OpenAI sibling is safe.
    // Uses `tokio::time::timeout` so a regression fails fast.
    #[tokio::test]
    async fn test_openai_body_image_url_attached() {
        // with 1 image data-URL, the last user message's content
        // must become an array containing a text part and an image_url part
        // whose url equals the supplied data-URL.
        let server = wiremock::MockServer::start().await;
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/event-stream")
                    .set_body_raw(sse.as_bytes(), "text/event-stream"),
            )
            .mount(&server)
            .await;

        let provider =
            OpenAIProvider::new("test-key".to_string(), server.uri(), "gpt-4o".to_string());
        let msgs = vec![ChatMessage {
            role: "user".to_string(),
            content: "what is in this image?".to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        }];
        let images = vec![
            "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
                .to_string(),
        ];
        let _ = provider
            .chat_with_tools(&msgs, &[], Effort::None, &images)
            .await
            .expect("chat_with_tools succeeds");

        let requests = server.received_requests().await.expect("requests captured");
        let body: serde_json::Value =
            serde_json::from_slice(&requests[0].body).expect("body is valid JSON");
        let messages = body["messages"].as_array().expect("messages is array");
        let last = messages.last().expect("has last message");
        let content = last["content"]
            .as_array()
            .expect("content is array (multimodal)");

        // Find the image_url part.
        let image_part = content
            .iter()
            .find(|p| p["type"].as_str() == Some("image_url"))
            .expect("image_url part present");
        let url = image_part["image_url"]["url"]
            .as_str()
            .expect("image_url.url is string");
        assert_eq!(url, images[0], "image_url must carry the supplied data-URL");

        // And a text part with the original prompt.
        let text_part = content
            .iter()
            .find(|p| p["type"].as_str() == Some("text"))
            .expect("text part present");
        assert_eq!(
            text_part["text"].as_str(),
            Some("what is in this image?"),
            "text part must carry the original prompt"
        );
    }

    #[tokio::test]
    async fn test_openai_body_no_image_when_empty() {
        // empty images slice must keep content as a plain string
        // (no multimodal array).
        let server = wiremock::MockServer::start().await;
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/event-stream")
                    .set_body_raw(sse.as_bytes(), "text/event-stream"),
            )
            .mount(&server)
            .await;

        let provider =
            OpenAIProvider::new("test-key".to_string(), server.uri(), "gpt-4o".to_string());
        let msgs = vec![ChatMessage {
            role: "user".to_string(),
            content: "plain text".to_string(),
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
        let messages = body["messages"].as_array().expect("messages is array");
        let last = messages.last().expect("has last message");
        assert!(
            last["content"].is_string(),
            "content must remain a plain string when no images. Got: {:?}",
            last["content"]
        );
    }

    #[test]
    fn test_prd115_image_attachment_helpers() {
        let img = ImageAttachment::from_bytes_with_name(
            Some("cat.png".to_string()),
            "image/png",
            b"\x89PNG\r\n",
        );
        assert_eq!(img.filename, Some("cat.png".to_string()));
        assert!(img.data_url.starts_with("data:image/png;base64,"));
        assert!(looks_like_image_data_url(&img.data_url));

        let u = ImageAttachment::from_data_url("data:image/jpeg;base64,ABC");
        assert_eq!(u.filename, None);
        assert_eq!(u.data_url, "data:image/jpeg;base64,ABC");

        assert_eq!(
            mime_from_path(std::path::Path::new("a/b/cat.PNG")),
            "image/png"
        );
        assert_eq!(mime_from_path(std::path::Path::new("x.jpg")), "image/jpeg");

        let msgs = vec![
            ChatMessage {
                role: "user".to_string(),
                content: "hi".to_string(),
                tool_call_id: None,
                tool_calls: None,
                attachments: Some(vec![ImageAttachment::from_data_url(
                    "data:image/png;base64,ZA==",
                )]),
                thinking_signature: None,
                redacted_thinking: None,
                thinking: None,
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: "ok".to_string(),
                tool_call_id: None,
                tool_calls: None,
                attachments: None,
                thinking_signature: None,
                redacted_thinking: None,
                thinking: None,
            },
        ];
        let urls = collect_turn_image_urls(&msgs);
        assert_eq!(urls, vec!["data:image/png;base64,ZA==".to_string()]);
    }
}
