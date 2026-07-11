//! E2E test for Anthropic Native Messages API Provider .
//!
//! AC-8: Mock Anthropic server, full `zz exec` flow with ZZ_PROVIDER=anthropic.

use assert_cmd::Command;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn e2e_prd11_ac8_anthropic_provider_full_flow() {
    let server = MockServer::start().await;

    // Anthropic SSE response: message_start + text delta + message_stop.
    let sse_response = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"role\":\"assistant\"}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello from Claude\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(sse_response.as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("ANTHROPIC_API_KEY", "sk-ant-test-key")
        .env("ANTHROPIC_BASE_URL", server.uri())
        .env("ZZ_PROVIDER", "anthropic")
        .env("ZZ_MODEL", "claude-sonnet-4-20250514")
        .env("ZZ_APPROVAL", "never")
        .env("ZZ_SANDBOX", "full-access")
        .args(["exec", "Say hello"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let s = String::from_utf8(output).expect("stdout is valid utf-8");
    let events: Vec<Value> = s
        .lines()
        .filter(|l| !l.is_empty())
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()
        .expect("every line is valid JSON");

    // Should have at least one item.updated with content from Claude.
    let has_content = events.iter().any(|e| {
        e.get("type").and_then(|t| t.as_str()) == Some("item.updated")
            && e.get("item")
                .and_then(|i| i.get("text"))
                .and_then(|t| t.as_str())
                .map(|t| t.contains("Hello from Claude"))
                .unwrap_or(false)
    });
    assert!(
        has_content,
        "Should have content from Anthropic provider. Events: {:?}",
        events
    );

    // Should end with turn.completed.
    let last_type = events
        .last()
        .and_then(|e| e.get("type").and_then(|t| t.as_str()));
    assert_eq!(
        last_type,
        Some("turn.completed"),
        "Should end with turn.completed. Last: {:?}",
        events.last()
    );
}
