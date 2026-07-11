//! Context compaction for ZeroZero .
//!
//! Simple sliding window + fallback summary compaction.
//! When messages exceed threshold, keep first user message + recent N,
//! replace middle with structured summary.

use zerozero_llm::ChatMessage;

/// Compaction configuration.
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// Max messages before compaction triggers (legacy count-based gate).
    pub max_messages: usize,
    /// Legacy max estimated tokens before compaction triggers.
    pub max_tokens: usize,
    /// Number of recent messages to keep verbatim (legacy).
    pub keep_recent: usize,
    /// Token budget for auto-compaction (Codex-style). When the total
    /// estimated token count of the conversation exceeds this, compaction
    /// triggers, keeping the most recent `keep_recent_turns` turns verbatim
    /// and summarizing all older turns into a single summary message.
    pub token_budget: usize,
    /// Number of recent turns to keep verbatim during token-budget compaction.
    pub keep_recent_turns: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            max_messages: 20,
            max_tokens: 100_000,
            keep_recent: 6,
            token_budget: 100_000,
            keep_recent_turns: 6,
        }
    }
}

/// Count tokens for a list of messages. Heuristic approximation: total
/// characters / 4 (no heavy tokenizer dependency). This is the canonical
/// token counter used by the token-budget auto-compaction path.
pub fn count_tokens(messages: &[ChatMessage]) -> usize {
    estimate_tokens(messages)
}

/// Estimate token count for a list of messages. Heuristic: chars / 4.
pub fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    let total_chars: usize = messages
        .iter()
        .map(|m| {
            let mut chars = m.content.chars().count();
            if let Some(tc) = &m.tool_calls {
                for call in tc {
                    chars += call.function.name.chars().count();
                    chars += call.function.arguments.chars().count();
                }
            }
            chars
        })
        .sum();
    total_chars / 4
}

/// Check if compaction should be triggered (legacy count/token gate).
pub fn should_compact(messages: &[ChatMessage], config: &CompactionConfig) -> bool {
    if messages.len() > config.max_messages {
        return true;
    }
    let tokens = estimate_tokens(messages);
    if tokens > config.max_tokens {
        return true;
    }
    false
}

/// Check if token-budget auto-compaction should trigger (Codex-style).
///
/// Triggers when the total estimated token count (`count_tokens`) exceeds
/// `config.token_budget` AND there are more messages than `keep_recent_turns`
/// (so we always have something to keep + something to summarize).
pub fn should_compact_token_budget(messages: &[ChatMessage], config: &CompactionConfig) -> bool {
    if messages.len() <= config.keep_recent_turns {
        return false;
    }
    let tokens = count_tokens(messages);
    tokens > config.token_budget
}

/// Token-budget auto-compaction (Codex-style).
///
/// Keeps the most recent `keep_recent_turns` messages verbatim and summarizes
/// all older messages into a single summary message at the front of the
/// result. The `summarize` closure produces the summary text, making this
/// fully testable without an LLM .
///
/// If there is nothing to summarize (≤ `keep_recent_turns` messages), the
/// input is returned unchanged.
pub fn compact_token_budget<F>(
    messages: Vec<ChatMessage>,
    config: &CompactionConfig,
    summarize: F,
) -> Vec<ChatMessage>
where
    F: FnOnce(&[ChatMessage]) -> String,
{
    let len = messages.len();
    if len <= config.keep_recent_turns {
        return messages;
    }
    let keep_start = len.saturating_sub(config.keep_recent_turns);
    let older = &messages[..keep_start];
    let recent = &messages[keep_start..];

    let summary = summarize(older);

    let mut result = Vec::with_capacity(1 + recent.len());
    result.push(ChatMessage {
        role: "system".to_string(),
        content: summary,
        tool_call_id: None,
        tool_calls: None,
        attachments: None,
        thinking_signature: None,
        redacted_thinking: None,
        thinking: None,
    });
    result.extend_from_slice(recent);
    result
}

/// Async token-budget auto-compaction using the LLM provider for the summary,
/// falling back to `fallback_summary` if the LLM call fails.
pub async fn compact_token_budget_with_llm(
    messages: Vec<ChatMessage>,
    config: &CompactionConfig,
    provider: &dyn zerozero_llm::Provider,
) -> Vec<ChatMessage> {
    let len = messages.len();
    if len <= config.keep_recent_turns {
        return messages;
    }
    let keep_start = len.saturating_sub(config.keep_recent_turns);
    let older = &messages[..keep_start];
    let recent = &messages[keep_start..];

    let summary = match llm_summary(provider, older).await {
        Ok(s) => s,
        Err(_) => fallback_summary(older),
    };

    let mut result = Vec::with_capacity(1 + recent.len());
    result.push(ChatMessage {
        role: "system".to_string(),
        content: summary,
        tool_call_id: None,
        tool_calls: None,
        attachments: None,
        thinking_signature: None,
        redacted_thinking: None,
        thinking: None,
    });
    result.extend_from_slice(recent);
    result
}

/// Compact messages: keep first user message + recent N, replace middle with summary.
pub fn compact_messages(messages: Vec<ChatMessage>, config: &CompactionConfig) -> Vec<ChatMessage> {
    let len = messages.len();
    if len <= config.keep_recent + 1 {
        return messages;
    }

    let tail_start = len.saturating_sub(config.keep_recent);

    // head = first message (usually user prompt = goal)
    let head = &messages[0];
    // middle = messages[1..tail_start]
    let middle = &messages[1..tail_start];
    // tail = messages[tail_start..]
    let tail = &messages[tail_start..];

    let summary = fallback_summary(middle);

    let mut result = Vec::with_capacity(2 + tail.len());
    result.push(head.clone());
    result.push(ChatMessage {
        role: "system".to_string(),
        content: summary,
        tool_call_id: None,
        tool_calls: None,
        attachments: None,
        thinking_signature: None,
        redacted_thinking: None,
        thinking: None,
    });
    result.extend(tail.iter().cloned());
    result
}

/// Generate a structured fallback summary without LLM call.
/// Extracts goal, key actions, and latest state.
pub fn fallback_summary(messages: &[ChatMessage]) -> String {
    let mut summary = String::from("## Conversation Summary\n\n");

    // Goal: first user message in the middle section
    if let Some(first_user) = messages.iter().find(|m| m.role == "user") {
        let goal = if first_user.content.len() > 200 {
            let safe_end = first_user.content.floor_char_boundary(200);
            format!("{}...", &first_user.content[..safe_end])
        } else {
            first_user.content.clone()
        };
        summary.push_str(&format!("**Goal:** {goal}\n\n"));
    }

    // Key Actions: extract tool calls from assistant messages
    summary.push_str("**Key Actions:**\n");
    let mut action_count = 0;
    for msg in messages {
        if msg.role == "assistant"
            && let Some(tool_calls) = &msg.tool_calls
        {
            for tc in tool_calls {
                if action_count < 10 {
                    let first_line = tc.function.arguments.lines().next().unwrap_or("");
                    summary.push_str(&format!("- {}: {first_line}\n", tc.function.name));
                    action_count += 1;
                }
            }
        }
    }
    if action_count == 0 {
        summary.push_str("- (no tool calls in compacted section)\n");
    }

    // Recent State: last assistant message content (truncated)
    summary.push_str("\n**Recent State:**\n");
    if let Some(last_assistant) = messages.iter().rev().find(|m| m.role == "assistant") {
        let state = if last_assistant.content.len() > 500 {
            let safe_end = last_assistant.content.floor_char_boundary(500);
            format!("{}...", &last_assistant.content[..safe_end])
        } else {
            last_assistant.content.clone()
        };
        summary.push_str(&state);
    } else {
        summary.push_str("(no assistant response in compacted section)");
    }

    summary
}

/// Compact messages using LLM summarization. Falls back to `fallback_summary`
/// if the LLM call fails.
///
/// This is opt-in: callers should use this when they have a Provider
/// available. The standard `compact_messages` still uses `fallback_summary`.
pub async fn compact_messages_with_llm(
    messages: Vec<ChatMessage>,
    config: &CompactionConfig,
    provider: &dyn zerozero_llm::Provider,
) -> Vec<ChatMessage> {
    // Same structure as compact_messages but uses LLM for the summary.
    let len = messages.len();
    if len <= config.keep_recent + 1 {
        return messages;
    }
    let tail_start = len.saturating_sub(config.keep_recent);
    let head = &messages[0];
    let middle = &messages[1..tail_start];
    let tail = &messages[tail_start..];

    let summary = match llm_summary(provider, middle).await {
        Ok(s) => s,
        Err(_) => fallback_summary(middle), // fallback on LLM error
    };

    let mut result = Vec::with_capacity(2 + tail.len());
    result.push(head.clone());
    result.push(ChatMessage {
        role: "system".to_string(),
        content: summary,
        tool_call_id: None,
        tool_calls: None,
        attachments: None,
        thinking_signature: None,
        redacted_thinking: None,
        thinking: None,
    });
    result.extend(tail.iter().cloned());
    result
}

/// Generate a conversation summary using the LLM provider.
async fn llm_summary(
    provider: &dyn zerozero_llm::Provider,
    messages: &[ChatMessage],
) -> anyhow::Result<String> {
    use futures::StreamExt;

    // Build a prompt that asks the LLM to summarize the conversation.
    let mut prompt = String::from(
        "Summarize the following conversation between a user and an AI coding assistant. \
         Focus on: the goal, key actions taken (tool calls, file edits, commands run), \
         and the current state. Be concise (max 500 words).\n\n",
    );
    for msg in messages {
        prompt.push_str(&format!("[{}]: ", msg.role));
        if msg.content.len() > 500 {
            let safe_end = msg.content.floor_char_boundary(500);
            prompt.push_str(&msg.content[..safe_end]);
            prompt.push_str("...");
        } else {
            prompt.push_str(&msg.content);
        }
        prompt.push('\n');
        if let Some(tool_calls) = &msg.tool_calls {
            for tc in tool_calls {
                prompt.push_str(&format!(
                    "  (tool_call: {} args: {})\n",
                    tc.function.name, tc.function.arguments
                ));
            }
        }
    }

    let mut stream = provider.chat_stream(&prompt).await?;
    let mut summary = String::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(delta) => summary.push_str(&delta),
            Err(e) => return Err(e),
        }
    }
    if summary.is_empty() {
        anyhow::bail!("LLM returned empty summary");
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_msg(content: &str) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: content.to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        }
    }

    fn assistant_msg(content: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: content.to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        }
    }

    #[test]
    fn test_estimate_tokens_basic() {
        let messages = vec![user_msg("hello world")]; // 11 chars / 4 = 2
        let tokens = estimate_tokens(&messages);
        assert_eq!(tokens, 2);
    }

    #[test]
    fn test_estimate_tokens_empty() {
        let messages: Vec<ChatMessage> = vec![];
        let tokens = estimate_tokens(&messages);
        assert_eq!(tokens, 0);
    }

    #[test]
    fn test_should_compact_by_messages() {
        let config = CompactionConfig {
            max_messages: 5,
            max_tokens: 100_000,
            keep_recent: 2,
            ..CompactionConfig::default()
        };
        let messages: Vec<ChatMessage> = (0..6).map(|i| user_msg(&format!("msg {i}"))).collect();
        assert!(should_compact(&messages, &config));
    }

    #[test]
    fn test_should_compact_by_tokens() {
        let config = CompactionConfig {
            max_messages: 100,
            max_tokens: 10,
            keep_recent: 2,
            ..CompactionConfig::default()
        };
        let messages = vec![user_msg(
            "this is a long message that exceeds the token limit",
        )];
        assert!(should_compact(&messages, &config));
    }

    #[test]
    fn test_should_not_compact() {
        let config = CompactionConfig {
            max_messages: 100,
            max_tokens: 100_000,
            keep_recent: 6,
            ..CompactionConfig::default()
        };
        let messages = vec![user_msg("hello"), assistant_msg("hi")];
        assert!(!should_compact(&messages, &config));
    }

    #[test]
    fn test_compact_messages_keeps_head_and_tail() {
        let config = CompactionConfig {
            max_messages: 5,
            max_tokens: 100_000,
            keep_recent: 2,
            ..CompactionConfig::default()
        };
        let messages: Vec<ChatMessage> = (0..10)
            .map(|i| {
                if i % 2 == 0 {
                    user_msg(&format!("user {i}"))
                } else {
                    assistant_msg(&format!("assistant {i}"))
                }
            })
            .collect();

        let result = compact_messages(messages, &config);
        // head (1) + summary (1) + tail (2) = 4
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].role, "user");
        assert_eq!(result[0].content, "user 0");
        assert_eq!(result[1].role, "system");
        assert!(result[1].content.contains("Conversation Summary"));
        assert_eq!(result[2].content, "user 8");
        assert_eq!(result[3].content, "assistant 9");
    }

    #[test]
    fn test_compact_messages_short_list() {
        let config = CompactionConfig::default();
        let messages = vec![user_msg("hello"), assistant_msg("hi")];
        let result = compact_messages(messages, &config);
        // Should not compact: 2 <= keep_recent + 1 = 7
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_fallback_summary_has_goal() {
        let messages = vec![
            user_msg("fix the bug in auth.rs"),
            assistant_msg("I'll look at the file"),
        ];
        let summary = fallback_summary(&messages);
        assert!(summary.contains("fix the bug in auth.rs"));
        assert!(summary.contains("**Goal:**"));
    }

    #[test]
    fn test_fallback_summary_has_actions() {
        use zerozero_llm::{ToolCall, ToolCallFunction};
        let messages = vec![ChatMessage {
            role: "assistant".to_string(),
            content: "Let me check".to_string(),
            tool_call_id: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                call_type: "function".to_string(),
                function: ToolCallFunction {
                    name: "bash".to_string(),
                    arguments: r#"{"command":"ls -la"}"#.to_string(),
                },
            }]),
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        }];
        let summary = fallback_summary(&messages);
        assert!(summary.contains("bash"));
        assert!(summary.contains("**Key Actions:**"));
    }

    #[test]
    fn test_fallback_summary_has_recent_state() {
        let messages = vec![
            user_msg("do something"),
            assistant_msg("I completed the task successfully"),
        ];
        let summary = fallback_summary(&messages);
        assert!(summary.contains("I completed the task"));
        assert!(summary.contains("**Recent State:**"));
    }

    // --- : Mutation coverage fix ---

    #[test]
    fn test_prd8_ac2_should_compact_boundary_messages() {
        // Boundary on message count: max_messages=20
        let cfg = CompactionConfig {
            max_messages: 20,
            max_tokens: 1_000_000,
            keep_recent: 6,
            ..CompactionConfig::default()
        };
        // 19 messages → false (off-by-one risk: `>` vs `>=`)
        let msgs19: Vec<ChatMessage> = (0..19).map(|_| user_msg("x")).collect();
        assert!(
            !should_compact(&msgs19, &cfg),
            "19 messages should NOT compact (max=20)"
        );
        // 20 → false (because `>` strict, not `>=`)
        let msgs20: Vec<ChatMessage> = (0..20).map(|_| user_msg("x")).collect();
        assert!(
            !should_compact(&msgs20, &cfg),
            "20 messages == max should NOT compact (rule is `> max`)"
        );
        // 21 → true
        let msgs21: Vec<ChatMessage> = (0..21).map(|_| user_msg("x")).collect();
        assert!(
            should_compact(&msgs21, &cfg),
            "21 messages should compact (max=20)"
        );
    }

    #[test]
    fn test_prd8_ac2_should_compact_boundary_tokens() {
        // Boundary on token estimate: max_tokens=100_000.
        // estimate_tokens = total_chars / 4.
        // 100_000 tokens = 400_000 chars.
        // We can't easily reach 400k chars with one message; use config with
        // small max_tokens.
        let cfg = CompactionConfig {
            max_messages: 1_000_000,
            max_tokens: 2,
            keep_recent: 6,
            ..CompactionConfig::default()
        };
        // 7 chars → 1 token → false
        let small = vec![user_msg("abcdefg")];
        assert!(!should_compact(&small, &cfg), "7 chars (1 token) < 2");
        // 8 chars → 2 tokens → false (`>` strict)
        let exact = vec![user_msg("abcdefgh")];
        assert!(
            !should_compact(&exact, &cfg),
            "8 chars (2 tokens) == max should NOT compact"
        );
        // 12 chars → 3 tokens → true
        let over = vec![user_msg("abcdefghijkl")];
        assert!(
            should_compact(&over, &cfg),
            "12 chars (3 tokens) > 2 should compact"
        );
    }

    #[test]
    fn test_prd8_ac3_estimate_tokens_division() {
        // Verify integer division semantics (chars / 4).
        assert_eq!(estimate_tokens(&[user_msg("")]), 0, "0 chars → 0");
        assert_eq!(estimate_tokens(&[user_msg("abcd")]), 1, "4 chars → 1");
        assert_eq!(
            estimate_tokens(&[user_msg("abcdefg")]),
            1,
            "7 chars → 1 (int div)"
        );
        assert_eq!(estimate_tokens(&[user_msg("abcdefgh")]), 2, "8 chars → 2");
        assert_eq!(
            estimate_tokens(&[user_msg(&"a".repeat(100))]),
            25,
            "100 chars → 25"
        );
    }

    #[test]
    fn test_prd8_ac3_estimate_tokens_multi_messages() {
        // Chars from multiple messages sum, then divide.
        // 3 messages of 4 chars each → 12 chars → 3 tokens.
        let msgs = vec![user_msg("abcd"), user_msg("efgh"), user_msg("ijkl")];
        assert_eq!(estimate_tokens(&msgs), 3);
    }

    // --- Token-budget auto-compaction (Codex parity F15) ---

    #[test]
    fn test_count_tokens_known_string() {
        // "hello world" = 11 chars → 2 tokens (chars / 4).
        let msgs = vec![user_msg("hello world")];
        assert_eq!(count_tokens(&msgs), 2);
        // Verify the canonical counter equals the legacy estimator.
        assert_eq!(count_tokens(&msgs), estimate_tokens(&msgs));
    }

    #[test]
    fn test_count_tokens_multi_message() {
        // "abcd" + "efgh" + "ijkl" = 12 chars → 3 tokens.
        let msgs = vec![user_msg("abcd"), user_msg("efgh"), user_msg("ijkl")];
        assert_eq!(count_tokens(&msgs), 3);
    }

    #[test]
    fn test_count_tokens_empty() {
        let msgs: Vec<ChatMessage> = vec![];
        assert_eq!(count_tokens(&msgs), 0);
    }

    #[test]
    fn test_should_compact_token_budget_triggers() {
        let cfg = CompactionConfig {
            token_budget: 10,
            keep_recent_turns: 2,
            ..CompactionConfig::default()
        };
        // 3 msgs of 40 chars each = 120 chars / 4 = 30 tokens > 10.
        let msgs: Vec<ChatMessage> = (0..3)
            .map(|i| user_msg(&format!("long message number {i} with padding")))
            .collect();
        assert!(should_compact_token_budget(&msgs, &cfg));
    }

    #[test]
    fn test_should_not_compact_token_budget_under_budget() {
        let cfg = CompactionConfig {
            token_budget: 100_000,
            keep_recent_turns: 2,
            ..CompactionConfig::default()
        };
        let msgs = vec![user_msg("hi"), user_msg("yo")];
        assert!(!should_compact_token_budget(&msgs, &cfg));
    }

    #[test]
    fn test_should_not_compact_token_budget_too_few_messages() {
        let cfg = CompactionConfig {
            token_budget: 1, // tiny budget
            keep_recent_turns: 6,
            ..CompactionConfig::default()
        };
        // Many tokens but ≤ keep_recent_turns messages → never compact.
        let msgs: Vec<ChatMessage> = (0..5).map(|_| user_msg(&"x".repeat(1000))).collect();
        assert!(!should_compact_token_budget(&msgs, &cfg));
    }

    #[test]
    fn test_compact_token_budget_keep_recent_and_summarize() {
        let cfg = CompactionConfig {
            token_budget: 10,
            keep_recent_turns: 3,
            ..CompactionConfig::default()
        };
        // 10 messages, keep last 3 verbatim, summarize first 7.
        let msgs: Vec<ChatMessage> = (0..10)
            .map(|i| {
                if i % 2 == 0 {
                    user_msg(&format!("user {i}"))
                } else {
                    assistant_msg(&format!("assistant {i}"))
                }
            })
            .collect();

        let result = compact_token_budget(msgs, &cfg, |older| {
            format!("SUMMARY of {} older messages", older.len())
        });

        // 1 summary + 3 recent = 4.
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].role, "system");
        assert_eq!(result[0].content, "SUMMARY of 7 older messages");
        // Recent are the last 3 (indices 7,8,9).
        assert_eq!(result[1].content, "assistant 7");
        assert_eq!(result[2].content, "user 8");
        assert_eq!(result[3].content, "assistant 9");
    }

    #[test]
    fn test_compact_token_budget_short_list_unchanged() {
        let cfg = CompactionConfig {
            keep_recent_turns: 6,
            ..CompactionConfig::default()
        };
        let msgs = vec![user_msg("a"), assistant_msg("b")];
        let result = compact_token_budget(msgs, &cfg, |_| panic!("summarize must not be called"));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "a");
    }

    #[test]
    fn test_compact_token_budget_no_recent_messages() {
        // keep_recent_turns = 0 → summarize everything, keep none.
        let cfg = CompactionConfig {
            token_budget: 1,
            keep_recent_turns: 0,
            ..CompactionConfig::default()
        };
        let msgs: Vec<ChatMessage> = (0..3).map(|i| user_msg(&format!("m{i}"))).collect();
        let result = compact_token_budget(msgs, &cfg, |older| {
            format!("all {} summarized", older.len())
        });
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].role, "system");
        assert_eq!(result[0].content, "all 3 summarized");
    }
}

#[cfg(test)]
mod tests_llm {
    use super::*;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};
    use zerozero_llm::{DeltaStream, Effort, Provider, SseEventStream};

    fn user_msg(content: &str) -> ChatMessage {
        ChatMessage {
            role: "user".to_string(),
            content: content.to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        }
    }

    fn assistant_msg(content: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: content.to_string(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        }
    }

    fn ten_messages() -> Vec<ChatMessage> {
        (0..10)
            .map(|i| {
                if i % 2 == 0 {
                    user_msg(&format!("user {i}"))
                } else {
                    assistant_msg(&format!("assistant {i}"))
                }
            })
            .collect()
    }

    // Mock provider that returns a fixed summary.
    struct MockProvider;

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat_stream(&self, _prompt: &str) -> anyhow::Result<DeltaStream> {
            let stream = futures::stream::iter(vec![Ok("LLM summary of conversation".to_string())]);
            Ok(Box::pin(stream))
        }
        async fn chat_with_tools(
            &self,
            _messages: &[ChatMessage],
            _tools: &[serde_json::Value],
            _effort: Effort,
            _images: &[String],
        ) -> anyhow::Result<SseEventStream> {
            unimplemented!()
        }
    }

    // Mock provider that always errors.
    struct ErrorProvider;

    #[async_trait]
    impl Provider for ErrorProvider {
        async fn chat_stream(&self, _prompt: &str) -> anyhow::Result<DeltaStream> {
            anyhow::bail!("LLM unavailable")
        }
        async fn chat_with_tools(
            &self,
            _messages: &[ChatMessage],
            _tools: &[serde_json::Value],
            _effort: Effort,
            _images: &[String],
        ) -> anyhow::Result<SseEventStream> {
            unimplemented!()
        }
    }

    // Mock provider that captures the prompt it receives.
    struct CapturingProvider {
        captured: Arc<Mutex<String>>,
    }

    #[async_trait]
    impl Provider for CapturingProvider {
        async fn chat_stream(&self, prompt: &str) -> anyhow::Result<DeltaStream> {
            *self.captured.lock().unwrap() = prompt.to_string();
            let stream = futures::stream::iter(vec![Ok("captured summary".to_string())]);
            Ok(Box::pin(stream))
        }
        async fn chat_with_tools(
            &self,
            _messages: &[ChatMessage],
            _tools: &[serde_json::Value],
            _effort: Effort,
            _images: &[String],
        ) -> anyhow::Result<SseEventStream> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn test_compact_with_llm_success() {
        let config = CompactionConfig {
            max_messages: 5,
            max_tokens: 100_000,
            keep_recent: 2,
            ..CompactionConfig::default()
        };
        let messages = ten_messages();
        let provider = MockProvider;
        let result = compact_messages_with_llm(messages, &config, &provider).await;
        // head (1) + summary (1) + tail (2) = 4
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].role, "user");
        assert_eq!(result[0].content, "user 0");
        assert_eq!(result[1].role, "system");
        assert!(
            result[1].content.contains("LLM summary"),
            "summary should contain LLM output"
        );
        assert_eq!(result[2].content, "user 8");
        assert_eq!(result[3].content, "assistant 9");
    }

    #[tokio::test]
    async fn test_compact_with_llm_falls_back_on_error() {
        let config = CompactionConfig {
            max_messages: 5,
            max_tokens: 100_000,
            keep_recent: 2,
            ..CompactionConfig::default()
        };
        let messages = ten_messages();
        let provider = ErrorProvider;
        let result = compact_messages_with_llm(messages, &config, &provider).await;
        // head (1) + fallback summary (1) + tail (2) = 4
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].role, "user");
        assert_eq!(result[1].role, "system");
        assert!(
            result[1].content.contains("Conversation Summary"),
            "fallback summary should be used on LLM error"
        );
        assert_eq!(result[2].content, "user 8");
        assert_eq!(result[3].content, "assistant 9");
    }

    #[tokio::test]
    async fn test_compact_with_llm_short_list() {
        let config = CompactionConfig::default();
        let messages = vec![user_msg("hello"), assistant_msg("hi")];
        let provider = MockProvider;
        let result = compact_messages_with_llm(messages, &config, &provider).await;
        // 2 <= keep_recent + 1 = 7 → no compaction
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "hello");
        assert_eq!(result[1].content, "hi");
    }

    #[tokio::test]
    async fn test_llm_summary_prompt_contains_roles() {
        let config = CompactionConfig {
            max_messages: 5,
            max_tokens: 100_000,
            keep_recent: 2,
            ..CompactionConfig::default()
        };
        let messages = ten_messages();
        let captured = Arc::new(Mutex::new(String::new()));
        let provider = CapturingProvider {
            captured: captured.clone(),
        };
        let _result = compact_messages_with_llm(messages, &config, &provider).await;
        let prompt = captured.lock().unwrap().clone();
        // The prompt should include role labels for the middle messages
        // (messages[1..8] = assistant 1, user 2, assistant 3, ... assistant 7).
        assert!(
            prompt.contains("[assistant]:"),
            "prompt should contain assistant role label: {prompt}"
        );
        assert!(
            prompt.contains("[user]:"),
            "prompt should contain user role label: {prompt}"
        );
        // Should contain the summarize instruction.
        assert!(
            prompt.contains("Summarize the following conversation"),
            "prompt should contain summarize instruction"
        );
        // Should contain content from middle messages, not head/tail.
        assert!(
            prompt.contains("assistant 1"),
            "prompt should contain middle message content"
        );
    }
}
