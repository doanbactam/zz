//! E2E tests for Reasoning Effort Control).
//!
//! Tests AC-2 through AC-5 via wiremock mock servers:
//! - `--effort high` sets `reasoning_effort` in OpenAI request body.
//! - Default (no `--effort`) sends no `reasoning_effort`.
//! - Denylisted models skip `reasoning_effort`.
//! - Anthropic `--effort high` sends `thinking.budget_tokens`.
//! - HTTP 400 triggers retry without effort param.
//! - Invalid effort value → exit 1.
//! - `ZZ_EFFORT` env var override.
//! - `--effort` flag overrides `ZZ_EFFORT` env.
//!
//! Written by B (Test Author, Round 7) based on + design only.

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use serde_json::Value;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// OpenAI SSE response with a single content delta + [DONE].
fn openai_sse() -> String {
    let mut body = String::new();
    body.push_str("data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n");
    body.push_str("data: [DONE]\n\n");
    body
}

/// Anthropic SSE response: message_start + text delta + message_stop.
fn anthropic_sse() -> String {
    let mut body = String::new();
    body.push_str("event: message_start\n");
    body.push_str("data: {\"type\":\"message_start\",\"message\":{}}\n\n");
    body.push_str("event: content_block_delta\n");
    body.push_str("data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n");
    body.push_str("event: message_stop\n");
    body.push_str("data: {\"type\":\"message_stop\"}\n\n");
    body
}

/// Extract the first request body as JSON from a mock server.
async fn first_request_body(server: &MockServer) -> Value {
    let requests = server
        .received_requests()
        .await
        .expect("requests should be captured");
    assert!(!requests.is_empty(), "at least one request expected");
    serde_json::from_slice(&requests[0].body).expect("body is valid JSON")
}

/// AC-4: `zz exec --effort high` with OpenAI provider → request body
/// contains top-level `reasoning_effort: "high"`.
#[tokio::test]
async fn e2e_effort_flag_sets_reasoning_effort() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(openai_sse().as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_PROVIDER", "openai")
        .env("ZZ_MODEL", "gpt-5")
        .env("ZZ_APPROVAL", "never")
        .env("ZZ_SANDBOX", "full-access")
        .args(["exec", "--no-skills", "--effort", "high", "test"])
        .assert()
        .success();

    let body = first_request_body(&server).await;
    assert_eq!(
        body["reasoning_effort"].as_str(),
        Some("high"),
        "body must contain reasoning_effort='high'. Got: {body}"
    );
}

/// AC-4: `zz exec` without `--effort` → body does NOT contain
/// `reasoning_effort` (default None, preserve behavior).
#[tokio::test]
async fn e2e_effort_default_none_no_param() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(openai_sse().as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_PROVIDER", "openai")
        .env("ZZ_MODEL", "gpt-5")
        .env("ZZ_APPROVAL", "never")
        .env("ZZ_SANDBOX", "full-access")
        .args(["exec", "--no-skills", "test"])
        .assert()
        .success();

    let body = first_request_body(&server).await;
    assert!(
        body.get("reasoning_effort").is_none(),
        "body must NOT contain reasoning_effort when --effort is not set. Got: {body}"
    );
}

/// AC-5: `zz exec --effort high --model grok-4` (exact) AND `grok-4-xxx`
/// (prefix) → body does NOT contain `reasoning_effort` (denylist skip).
/// Design §2.3: exact `grok-4` + prefix `grok-4-*` both denied.
#[tokio::test]
async fn e2e_effort_denylist_grok4() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(openai_sse().as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    // Test exact `grok-4` (xAI default model — must be denied).
    Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_PROVIDER", "openai")
        .env("ZZ_APPROVAL", "never")
        .env("ZZ_SANDBOX", "full-access")
        .args([
            "exec",
            "--no-skills",
            "--effort",
            "high",
            "--model",
            "grok-4",
            "test",
        ])
        .assert()
        .success();

    let body = first_request_body(&server).await;
    assert!(
        body.get("reasoning_effort").is_none(),
        "body must NOT contain reasoning_effort for exact denylisted model grok-4. Got: {body}"
    );

    // Also test prefix `grok-4-xxx`.
    let server2 = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(openai_sse().as_bytes(), "text/event-stream"),
        )
        .mount(&server2)
        .await;

    Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_BASE_URL", server2.uri())
        .env("ZZ_PROVIDER", "openai")
        .env("ZZ_APPROVAL", "never")
        .env("ZZ_SANDBOX", "full-access")
        .args([
            "exec",
            "--no-skills",
            "--effort",
            "high",
            "--model",
            "grok-4-xxx",
            "test",
        ])
        .assert()
        .success();

    let body2 = first_request_body(&server2).await;
    assert!(
        body2.get("reasoning_effort").is_none(),
        "body must NOT contain reasoning_effort for prefix denylisted model grok-4-xxx. Got: {body2}"
    );
}

/// AC-3: `zz exec --effort high --provider anthropic` → body contains
/// `thinking.budget_tokens` and `max_tokens = budget + 4096`.
#[tokio::test]
async fn e2e_effort_anthropic_thinking() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(anthropic_sse().as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    Command::cargo_bin("zz")
        .unwrap()
        .env("ANTHROPIC_API_KEY", "sk-ant-test")
        .env("ANTHROPIC_BASE_URL", server.uri())
        .env("ZZ_PROVIDER", "anthropic")
        .env("ZZ_MODEL", "claude-opus-4")
        .env("ZZ_APPROVAL", "never")
        .env("ZZ_SANDBOX", "full-access")
        .args(["exec", "--no-skills", "--effort", "high", "test"])
        .assert()
        .success();

    let body = first_request_body(&server).await;
    assert_eq!(
        body["thinking"]["type"].as_str(),
        Some("enabled"),
        "thinking.type must be 'enabled'. Got: {body}"
    );
    assert_eq!(
        body["thinking"]["budget_tokens"].as_u64(),
        Some(16000),
        "thinking.budget_tokens must be 16000 for High. Got: {body}"
    );
    //B: max_tokens = budget + 4096 = 20096.
    assert_eq!(
        body["max_tokens"].as_u64(),
        Some(20096),
        "max_tokens must be 20096 (16000+4096). Got: {body}"
    );
}

/// AC-5: Mock returns 400 for request with reasoning_effort, 200 for
/// request without → ZeroZero retries, exit 0.
#[tokio::test]
async fn e2e_effort_400_retry() {
    let server = MockServer::start().await;

    // First request (with reasoning_effort) → 400.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains("reasoning_effort"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"error":{"message":"reasoning_effort not supported for this model"}}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    // Second request (without reasoning_effort) → 200.
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(openai_sse().as_bytes(), "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;

    Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_PROVIDER", "openai")
        .env("ZZ_MODEL", "gpt-5")
        .env("ZZ_APPROVAL", "never")
        .env("ZZ_SANDBOX", "full-access")
        .args(["exec", "--no-skills", "--effort", "high", "test"])
        .assert()
        .success();

    // Verify two requests: first with reasoning_effort, second without.
    let requests = server.received_requests().await.expect("requests captured");
    assert_eq!(requests.len(), 2, "exactly 2 requests (400 + retry 200)");
    let body1: Value = serde_json::from_slice(&requests[0].body).expect("body1 JSON");
    let body2: Value = serde_json::from_slice(&requests[1].body).expect("body2 JSON");
    assert!(
        body1.get("reasoning_effort").is_some(),
        "first has reasoning_effort"
    );
    assert!(
        body2.get("reasoning_effort").is_none(),
        "retry has no reasoning_effort"
    );
}

/// AC-4: `zz exec --effort invalid` → exit 1, error message.
#[test]
fn e2e_effort_invalid_value() {
    Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("ZZ_MODEL", "gpt-5")
        .env("ZZ_APPROVAL", "never")
        .env("ZZ_SANDBOX", "full-access")
        .args(["exec", "--no-skills", "--effort", "invalid", "test"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("invalid").or(predicates::str::contains("effort")));
}

/// AC-4: `ZZ_EFFORT=high zz exec "test"` → body contains
/// `reasoning_effort: "high"` (env override).
#[tokio::test]
async fn e2e_effort_env_override() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(openai_sse().as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_PROVIDER", "openai")
        .env("ZZ_MODEL", "gpt-5")
        .env("ZZ_APPROVAL", "never")
        .env("ZZ_SANDBOX", "full-access")
        .env("ZZ_EFFORT", "high")
        .args(["exec", "--no-skills", "test"])
        .assert()
        .success();

    let body = first_request_body(&server).await;
    assert_eq!(
        body["reasoning_effort"].as_str(),
        Some("high"),
        "ZZ_EFFORT=high must set reasoning_effort='high'. Got: {body}"
    );
}

/// AC-4: `ZZ_EFFORT=low zz exec --effort high` → body contains
/// `reasoning_effort: "high"` (flag wins over env).
#[tokio::test]
async fn e2e_effort_flag_overrides_env() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(openai_sse().as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_PROVIDER", "openai")
        .env("ZZ_MODEL", "gpt-5")
        .env("ZZ_APPROVAL", "never")
        .env("ZZ_SANDBOX", "full-access")
        .env("ZZ_EFFORT", "low")
        .args(["exec", "--no-skills", "--effort", "high", "test"])
        .assert()
        .success();

    let body = first_request_body(&server).await;
    assert_eq!(
        body["reasoning_effort"].as_str(),
        Some("high"),
        "flag --effort high must override ZZ_EFFORT=low. Got: {body}"
    );
}
