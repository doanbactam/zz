//! E2E tests for /model Slash Command TUI Mid-Session).
//!
//! Tests AC-2 and AC-3:
//! - AC-3: `zz exec --model <name>` sends the model name in the API request
//!   body to the mock server. This tests the `build_provider_with_model`
//!   function via the CLI `--model` flag path (env var override). The TUI
//!   `/model` slash command factory closure path
//!   (`build_provider_with_model(Some(model))`) is tested by the unit tests
//!   in `crates/cli/src/main.rs::tests`.
//! - AC-2: `/model` (no arg) shows the current model. This is a TUI-only
//!   behavior (rendered in the chat area) and is fully covered by the unit
//!   tests in `crates/tui/src/app.rs` (`test_model_no_arg_shows_current`) and
//!   `crates/tui/src/slash.rs` (`test_parse_model_empty`). The E2E test here
//!   is a no-op marker that documents that coverage.
//!
//! Written by B (Test Author, Round 7) based on + design only.

use assert_cmd::Command;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// OpenAI SSE response with a single content delta + [DONE].
fn openai_sse() -> String {
    let mut body = String::new();
    body.push_str("data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n");
    body.push_str("data: [DONE]\n\n");
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

/// AC-3: `zz exec --model <name>` sends the model name in the API request
/// body. This tests the `build_provider_with_model` function via the CLI
/// `--model` flag path (env var override). The TUI `/model` slash command
/// factory closure path (`build_provider_with_model(Some(model))`) is
/// tested by unit tests in `crates/cli/src/main.rs::tests`.
#[tokio::test]
async fn e2e_prd100_model_switch() {
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
        .env("ZZ_APPROVAL", "never")
        .env("ZZ_SANDBOX", "full-access")
        .args(["exec", "--no-skills", "--model", "grok-4.3", "hi"])
        .assert()
        .success();

    let body = first_request_body(&server).await;
    assert_eq!(
        body["model"].as_str(),
        Some("grok-4.3"),
        "request body must contain model='grok-4.3'. Got: {body}"
    );

    // Second run with a different model to ensure the value is dynamic
    // (mutation-resistant — not a hardcoded constant in the request).
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
        .args(["exec", "--no-skills", "--model", "o3-mini", "hi"])
        .assert()
        .success();

    let body2 = first_request_body(&server2).await;
    assert_eq!(
        body2["model"].as_str(),
        Some("o3-mini"),
        "request body must contain model='o3-mini'. Got: {body2}"
    );
}

// AC-2 (`/model` no-arg shows current model) is TUI-only and fully covered
// by the unit tests `crates/tui/src/app.rs::test_model_no_arg_shows_current`
// and `crates/tui/src/slash.rs::test_parse_model_empty`. Per (Cycle
// 100 retro) the no-op E2E stub that previously lived here was removed — no
// false confidence. The `e2e_prd100_model_switch` test above covers AC-3.
